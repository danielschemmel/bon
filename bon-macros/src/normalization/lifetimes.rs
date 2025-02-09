use crate::util::prelude::*;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;

#[derive(Default)]
pub(crate) struct NormalizeLifetimes;

impl VisitMut for NormalizeLifetimes {
    fn visit_item_impl_mut(&mut self, impl_block: &mut syn::ItemImpl) {
        syn::visit_mut::visit_item_impl_mut(self, impl_block);

        AssignLifetimes::new("i", &mut impl_block.generics).visit_type_mut(&mut impl_block.self_ty);
    }

    fn visit_impl_item_fn_mut(&mut self, fn_item: &mut syn::ImplItemFn) {
        // We are interested only in signatures of functions. Don't recurse
        // into the function's block.
        self.visit_signature_mut(&mut fn_item.sig);
    }

    fn visit_signature_mut(&mut self, signature: &mut syn::Signature) {
        let mut visitor = AssignLifetimes::new("f", &mut signature.generics);
        for arg in &mut signature.inputs {
            visitor.visit_fn_arg_mut(arg);
        }

        let syn::ReturnType::Type(_, return_type) = &mut signature.output else {
            return;
        };

        // Now perform lifetime elision for the lifetimes in the return type.
        // This code implements the logic described in the Rust reference:
        // https://doc.rust-lang.org/reference/lifetime-elision.html

        let elided_output_lifetime = signature
            .inputs
            .first()
            .and_then(|arg| {
                let receiver = arg.as_receiver()?;
                receiver.lifetime().or_else(|| {
                    let syn::Type::Reference(reference) = receiver.ty.as_ref() else {
                        return None;
                    };
                    reference.lifetime.as_ref()
                })
            })
            .or_else(|| {
                let lifetime = signature
                    .inputs
                    .iter()
                    .filter_map(syn::FnArg::as_typed)
                    .fold(LifetimeCollector::None, |mut acc, pat_type| {
                        acc.visit_pat_type(pat_type);
                        acc
                    });

                match lifetime {
                    LifetimeCollector::Single(lifetime) => Some(lifetime),
                    _ => None,
                }
            });

        let Some(elided_lifetime) = elided_output_lifetime else {
            return;
        };

        ElideOutputLifetime { elided_lifetime }.visit_type_mut(return_type)
    }
}

struct AssignLifetimes<'a> {
    prefix: &'static str,
    generics: &'a mut syn::Generics,
    next_lifetime_index: usize,
}

impl<'a> AssignLifetimes<'a> {
    fn new(prefix: &'static str, generics: &'a mut syn::Generics) -> Self {
        Self {
            prefix,
            generics,
            next_lifetime_index: 0,
        }
    }
}

impl VisitMut for AssignLifetimes<'_> {
    fn visit_item_mut(&mut self, _item: &mut syn::Item) {
        // Don't recurse into nested items because lifetimes aren't available there.
    }

    fn visit_type_bare_fn_mut(&mut self, _bare_fn: &mut syn::TypeBareFn) {
        // Skip function pointers because anon lifetimes that appear in them
        // don't belong to the surrounding function signature.
    }

    fn visit_parenthesized_generic_arguments_mut(
        &mut self,
        _args: &mut syn::ParenthesizedGenericArguments,
    ) {
        // Skip Fn traits for the same reason as function pointers described higher.
    }

    fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
        if lifetime.ident == "_" {
            *lifetime = self.next_lifetime();
        }
    }

    fn visit_type_reference_mut(&mut self, reference: &mut syn::TypeReference) {
        syn::visit_mut::visit_type_reference_mut(self, reference);
        reference
            .lifetime
            .get_or_insert_with(|| self.next_lifetime());
    }

    fn visit_receiver_mut(&mut self, receiver: &mut syn::Receiver) {
        // If this is a `self: Type` syntax, then it's not a special case
        // and we can just visit the explicit type of the receiver as usual
        if receiver.colon_token.is_some() {
            syn::visit_mut::visit_type_mut(self, &mut receiver.ty);
            return;
        }

        let Some((_and, lifetime)) = &mut receiver.reference else {
            return;
        };

        if matches!(lifetime, Some(lifetime) if lifetime.ident != "_") {
            return;
        }

        let syn::Type::Reference(receiver_ty) = receiver.ty.as_mut() else {
            return;
        };

        let new_lifetime = self.next_lifetime();

        *lifetime = Some(new_lifetime.clone());

        receiver_ty.lifetime = Some(new_lifetime);
    }
}

impl AssignLifetimes<'_> {
    /// Make a lifetime with the next index. It's used to generate unique
    /// lifetimes for every occurrence of a reference with the anonymous
    /// lifetime.
    fn next_lifetime(&mut self) -> syn::Lifetime {
        let index = self.next_lifetime_index;
        self.next_lifetime_index += 1;

        let lifetime = format!("'__{}{index}", self.prefix);
        let lifetime = syn::Lifetime::new(&lifetime, proc_macro2::Span::call_site());

        let lifetime_param = syn::LifetimeParam::new(lifetime.clone());
        let lifetime_param = syn::GenericParam::Lifetime(lifetime_param);
        self.generics.params.insert(index, lifetime_param);

        lifetime
    }
}

enum LifetimeCollector<'a> {
    None,
    Single(&'a syn::Lifetime),
    Multiple,
}

impl<'a> Visit<'a> for LifetimeCollector<'a> {
    fn visit_item(&mut self, _item: &syn::Item) {
        // Don't recurse into nested items because lifetimes aren't available there.
    }

    fn visit_type_bare_fn(&mut self, _bare_fn: &syn::TypeBareFn) {
        // Skip function pointers because anon lifetimes that appear in them
        // don't belong to the surrounding function signature.
    }

    fn visit_parenthesized_generic_arguments(
        &mut self,
        _args: &syn::ParenthesizedGenericArguments,
    ) {
        // Skip Fn traits for the same reason as function pointers described higher.
    }

    fn visit_lifetime(&mut self, lifetime: &'a syn::Lifetime) {
        match self {
            Self::None => *self = Self::Single(lifetime),
            Self::Single(_) => *self = Self::Multiple,
            Self::Multiple => {}
        }
    }
}

struct ElideOutputLifetime<'a> {
    elided_lifetime: &'a syn::Lifetime,
}

impl VisitMut for ElideOutputLifetime<'_> {
    fn visit_item_mut(&mut self, _item: &mut syn::Item) {
        // Don't recurse into nested items because lifetimes aren't available there.
    }

    fn visit_type_bare_fn_mut(&mut self, _bare_fn: &mut syn::TypeBareFn) {
        // Skip function pointers because anon lifetimes that appear in them
        // don't belong to the surrounding function signature.
    }

    fn visit_parenthesized_generic_arguments_mut(
        &mut self,
        _args: &mut syn::ParenthesizedGenericArguments,
    ) {
        // Skip Fn traits for the same reason as function pointers described higher.
    }

    fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
        if lifetime.ident == "_" {
            *lifetime = self.elided_lifetime.clone();
        }
    }

    fn visit_type_reference_mut(&mut self, reference: &mut syn::TypeReference) {
        syn::visit_mut::visit_type_reference_mut(self, reference);

        reference
            .lifetime
            .get_or_insert_with(|| self.elided_lifetime.clone());
    }
}
