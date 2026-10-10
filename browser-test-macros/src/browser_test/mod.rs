//! The expansion of `#[browser_test]`: a unit struct implementing `BrowserTest` by running the
//! annotated function.

mod arguments;
mod description;
mod signature;

use heck::ToUpperCamelCase;
use manyhow::{ErrorMessage, bail, ensure, error_message};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::{ItemFn, Visibility, ext::IdentExt, spanned::Spanned as _};

pub(crate) use arguments::Arguments;
use signature::{Inputs, TestSignature};

/// Struct names that would shadow the prelude's `Option` and `Result` variants: a unit struct also
/// defines a constant of its name.
const RESERVED_NAMES: &[&str] = &["None", "Some", "Ok", "Err"];

pub(crate) fn expand(args: Arguments, mut function: ItemFn) -> manyhow::Result<TokenStream> {
    let TestSignature {
        result,
        context,
        generics,
        inputs,
    } = TestSignature::parse(&function.sig)?;
    let crate_path = crate_path()?;
    let function_name = function.sig.ident.unraw().to_string();
    let name = struct_name(&function.sig.ident, &function_name)?;
    let test_name = args.name.map_or_else(
        || quote!(concat!(module_path!(), "::", #function_name)),
        |name| quote!(#name),
    );
    // The struct documents the test. The body keeps every other attribute: one meant for the
    // function, e.g. `#[expect(...)]` or `#[tracing::instrument]`, could fail on the struct.
    let docs: Vec<_> = function
        .attrs
        .extract_if(.., |attr| attr.path().is_ident("doc"))
        .collect();
    let description = description::expand(&docs);
    // Conditional compilation must guard every generated item, including the implementation.
    let cfgs: Vec<_> = function
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .cloned()
        .collect();
    let visibility = std::mem::replace(&mut function.vis, Visibility::Inherited);
    function.sig.ident = format_ident!("__browser_test_body");
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    // `async_trait` names every lifetime elided in `run`'s arguments, including the higher-ranked
    // ones in `fn(&str)` or `Fn(&str)`. The alias hides them.
    let context_alias = format_ident!("__{name}Context");
    // Spanned at the return type, where a wrong one is reported.
    let error = quote_spanned! {result.span()=>
        <#result as #crate_path::__private::TestResult>::Error
    };
    let call_args = match inputs {
        Inputs::None => quote!(),
        Inputs::Context => quote!(__browser_test_context),
        Inputs::DriverAndContext => quote!(__browser_test_driver, __browser_test_context),
    };
    Ok(quote! {
        #(#docs)*
        #(#cfgs)*
        #[derive(
            ::std::fmt::Debug,
            ::std::clone::Clone,
            ::std::marker::Copy,
            ::std::default::Default,
        )]
        #visibility struct #name;

        #(#cfgs)*
        impl #name {
            #function
        }

        #(#cfgs)*
        #[doc(hidden)]
        type #context_alias #type_generics = #context;

        #(#cfgs)*
        #[#crate_path::async_trait]
        impl #impl_generics #crate_path::BrowserTest<#context, #error> for #name #where_clause {
            fn name(&self) -> ::std::borrow::Cow<'_, str> {
                ::std::borrow::Cow::Borrowed(#test_name)
            }

            fn description(&self) -> ::std::option::Option<::std::borrow::Cow<'_, str>> {
                #description
            }

            async fn run(
                &self,
                __browser_test_driver: &#crate_path::thirtyfour::WebDriver,
                __browser_test_context: &#context_alias #type_generics,
            ) -> #result {
                Self::__browser_test_body(#call_args).await
            }
        }
    })
}

/// The path to `browser-test`, under the name the caller's `Cargo.toml` gives it.
fn crate_path() -> manyhow::Result<TokenStream> {
    match proc_macro_crate::crate_name("browser-test") {
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let name = format_ident!("{name}");
            Ok(quote!(::#name))
        }
        // `browser-test` declares `extern crate self as browser_test;` for its own tests.
        Ok(proc_macro_crate::FoundCrate::Itself) => Ok(quote!(::browser_test)),
        Err(error) => bail!(ErrorMessage::call_site(error)),
    }
}

/// The `PascalCase` name of the test's struct, spanned at the function's name.
fn struct_name(function: &Ident, function_name: &str) -> manyhow::Result<Ident> {
    let struct_name = function_name.to_upper_camel_case();
    ensure!(
        !RESERVED_NAMES.contains(&struct_name.as_str()),
        function,
        "a browser test named `{function_name}` would define `struct {struct_name};`, shadowing \
         the prelude's `{struct_name}`";
        help = "rename the function, and keep the test's name with `#[browser_test(name = \"...\")]`",
    );
    let mut name = syn::parse_str::<Ident>(&struct_name).map_err(|_| {
        error_message!(
            function,
            "function name must produce a valid PascalCase struct name"
        )
    })?;
    name.set_span(function.span());
    Ok(name)
}

#[cfg(test)]
mod tests {
    use manyhow::ToTokensError;

    use super::{Arguments, expand};

    #[test]
    fn invalid_signatures_explain_the_unsupported_part() {
        for (source, message) in [
            (
                "fn test(_: &()) -> Result<(), Report> { Ok(()) }",
                "browser tests must be async functions",
            ),
            (
                "async unsafe fn test() -> Result<(), Report> { Ok(()) }",
                "browser tests must be safe Rust functions",
            ),
            (
                "async fn test<T>(_: &T) -> Result<(), Report> { Ok(()) }",
                "browser tests cannot have type or const parameters",
            ),
            (
                "async fn test(_: &(), _: &(), _: &()) -> Result<(), Report> { Ok(()) }",
                "expected no arguments, &Context, or &WebDriver and &Context",
            ),
            (
                "async fn test(_: &mut ()) -> Result<(), Report> { Ok(()) }",
                "browser test arguments must be shared references",
            ),
            (
                "async fn test(_: ()) -> Result<(), Report> { Ok(()) }",
                "browser test arguments must be shared references",
            ),
            (
                "async fn test(&self) -> Result<(), Report> { Ok(()) }",
                "browser tests must be free functions",
            ),
            (
                "async fn test() {}",
                "browser tests must return Result<(), Report>",
            ),
            (
                "async fn none() -> Result<(), Report> { Ok(()) }",
                "shadowing the prelude's `None`",
            ),
            (
                "async fn r#ok() -> Result<(), Report> { Ok(()) }",
                "shadowing the prelude's `Ok`",
            ),
            (
                "async fn __() -> Result<(), Report> { Ok(()) }",
                "function name must produce a valid PascalCase struct name",
            ),
        ] {
            let function = syn::parse_str(source).unwrap();
            let error = expand(Arguments::default(), function)
                .unwrap_err()
                .into_token_stream();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}
