//! The function signatures `#[browser_test]` supports.

use manyhow::{bail, ensure};
use proc_macro2::Span;
use syn::{
    FnArg, GenericParam, Generics, Lifetime, LifetimeParam, ParenthesizedGenericArguments,
    ReturnType, Safety, Signature, Type, TypeFnPtr, TypeReference,
    visit_mut::{self, VisitMut},
};

/// What the generated test needs from a supported signature.
pub(super) struct TestSignature {
    /// The declared return type, e.g. `Result<(), Report>` or an alias of it.
    pub(super) result: Type,
    /// The type behind the context reference, `()` without one. Its elided lifetimes are named.
    pub(super) context: Type,
    /// The function's lifetime parameters and those named in `context`.
    pub(super) generics: Generics,
    /// The arguments the function takes.
    pub(super) inputs: Inputs,
}

/// The arguments a browser test function takes.
pub(super) enum Inputs {
    None,
    Context,
    DriverAndContext,
}

impl TestSignature {
    pub(super) fn parse(signature: &Signature) -> manyhow::Result<Self> {
        ensure!(
            signature.asyncness.is_some(),
            signature,
            "browser tests must be async functions"
        );
        ensure!(
            !matches!(signature.safety, Safety::Unsafe(_))
                && signature.abi.is_none()
                && signature.variadic.is_none(),
            signature,
            "browser tests must be safe Rust functions"
        );
        if let Some(parameter) = signature
            .generics
            .params
            .iter()
            .find(|parameter| !matches!(parameter, GenericParam::Lifetime(_)))
        {
            bail!(
                parameter,
                "browser tests cannot have type or const parameters"
            );
        }
        let inputs = match signature.inputs.len() {
            0 => Inputs::None,
            1 => Inputs::Context,
            2 => Inputs::DriverAndContext,
            _ => bail!(
                &signature.inputs,
                "expected no arguments, &Context, or &WebDriver and &Context"
            ),
        };
        let mut context = None;
        for input in &signature.inputs {
            let FnArg::Typed(input) = input else {
                bail!(input, "browser tests must be free functions");
            };
            let reference = match input.ty.as_ref() {
                Type::Reference(reference) if reference.mutability.is_none() => reference,
                _ => bail!(
                    &input.ty,
                    "browser test arguments must be shared references"
                ),
            };
            // The last argument is the context.
            context = Some((*reference.elem).clone());
        }
        let ReturnType::Type(_, result) = &signature.output else {
            bail!(signature, "browser tests must return Result<(), Report>");
        };

        let mut context = context.unwrap_or_else(|| syn::parse_quote!(()));
        let mut generics = signature.generics.clone();
        generics.params.extend(
            name_elided_lifetimes(&mut context)
                .into_iter()
                .map(GenericParam::Lifetime),
        );
        Ok(Self {
            result: (**result).clone(),
            context,
            generics,
            inputs,
        })
    }
}

/// Names the elided lifetimes in `context`, returning the lifetime parameters to declare.
///
/// `Page<'_>` is valid on a function, but an impl needs a named lifetime shared by its context and
/// run method. Nested borrowed context fields may need several lifetimes.
fn name_elided_lifetimes(context: &mut Type) -> Vec<LifetimeParam> {
    let mut lifetimes = ElidedLifetimes::default();
    lifetimes.visit_type_mut(context);
    lifetimes.parameters
}

#[derive(Default)]
struct ElidedLifetimes {
    parameters: Vec<LifetimeParam>,
}

impl ElidedLifetimes {
    fn fresh(&mut self) -> Lifetime {
        let lifetime = Lifetime::new(
            &format!("'__browser_test_context_{}", self.parameters.len()),
            Span::mixed_site(),
        );
        self.parameters.push(LifetimeParam::new(lifetime.clone()));
        lifetime
    }
}

impl VisitMut for ElidedLifetimes {
    fn visit_lifetime_mut(&mut self, lifetime: &mut Lifetime) {
        if lifetime.ident == "_" {
            *lifetime = self.fresh();
        }
    }

    fn visit_type_reference_mut(&mut self, reference: &mut TypeReference) {
        if reference.lifetime.is_none() {
            reference.lifetime = Some(self.fresh());
        }
        visit_mut::visit_type_reference_mut(self, reference);
    }

    // Lifetimes elided in `fn(&str)` and `Fn(&str)` are higher-ranked, not parameters of the impl.
    fn visit_type_fn_ptr_mut(&mut self, _: &mut TypeFnPtr) {}

    fn visit_parenthesized_generic_arguments_mut(&mut self, _: &mut ParenthesizedGenericArguments) {
    }
}
