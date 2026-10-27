//! The expansion of `#[browser_test]`: a unit struct implementing `BrowserTest` by running the
//! annotated function.

mod arguments;
mod description;
mod signature;

pub(crate) use arguments::Arguments;
use heck::ToUpperCamelCase;
use manyhow::{ErrorMessage, bail, ensure, error_message};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use signature::{Inputs, TestSignature};
use syn::{ItemFn, Visibility, ext::IdentExt, spanned::Spanned as _};

/// Struct names that would shadow a name of the standard prelude (Rust 2024) in the test's module:
/// its types and traits, and the `Option` and `Result` variants, as a unit struct also defines a
/// constant of its name.
const RESERVED_NAMES: &[&str] = &[
    "AsMut",
    "AsRef",
    "AsyncFn",
    "AsyncFnMut",
    "AsyncFnOnce",
    "Box",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "Err",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
];

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
    // Only a test with tags has tags of its own.
    let tags = args.tags.filter(|tags| !tags.is_empty()).map(|tags| {
        quote! {
            fn tags(&self) -> ::std::vec::Vec<::std::borrow::Cow<'_, str>> {
                ::std::vec![#(::std::borrow::Cow::Borrowed(#tags)),*]
            }
        }
    });
    // Only a test with a time limit has settings of its own. Spanned at the expression, where one
    // of another type is reported.
    let settings = args.time_limit.map(|limit| {
        let settings = quote_spanned! {limit.span()=>
            #crate_path::TestSettings::new().with_time_limit(#limit)
        };
        quote! {
            fn settings(&self) -> #crate_path::TestSettings {
                #settings
            }
        }
    });
    let test_name = args.name.map_or_else(
        // Qualified, as the caller's module may define macros of these names.
        || quote!(::core::concat!(::core::module_path!(), "::", #function_name)),
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
    // The function keeps its name, which failure reports and `#[tracing::instrument]` show, and
    // the struct gets its visibility.
    let visibility = std::mem::replace(&mut function.vis, Visibility::Inherited);
    let body = &function.sig.ident;
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    // `async_trait` names every lifetime elided in `run`'s arguments, including the higher-ranked
    // ones in `fn(&str)` or `Fn(&str)`. The alias hides them.
    let context_alias = format_ident!("__{name}Context");
    let call_args = match inputs {
        Inputs::None => quote!(),
        Inputs::Context => quote!(__browser_test_context),
        Inputs::DriverAndContext => quote!(__browser_test_driver, __browser_test_context),
    };
    // Spanned at the return type, where a wrong one is reported.
    let into_dynamic = quote_spanned! {result.span()=>
        <#result as #crate_path::__private::TestResult>::into_dynamic
    };
    // Spanned at the function's name, where a future that is not `Send` is reported.
    let boxed_body = quote_spanned! {body.span()=>
        ::std::boxed::Box::pin(#crate_path::__private::run_body(#body(#call_args), #into_dynamic))
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

        #function

        #(#cfgs)*
        #[doc(hidden)]
        type #context_alias #type_generics = #context;

        // `run` returns the function's future without an `async` block of its own (as
        // `async_trait` would add one), only wrapped in browser-test's `run_body`, so that no frame
        // of generated code is on the stack while the test runs, and failure reports show the
        // function as the test's outermost frame.
        #(#cfgs)*
        impl #impl_generics #crate_path::BrowserTest<#context> for #name #where_clause {
            fn name(&self) -> ::std::borrow::Cow<'_, str> {
                ::std::borrow::Cow::Borrowed(#test_name)
            }

            fn description(&self) -> ::std::option::Option<::std::borrow::Cow<'_, str>> {
                #description
            }

            #tags

            #settings

            fn run<'__self, '__driver, '__context, '__future>(
                &'__self self,
                __browser_test_driver: &'__driver #crate_path::thirtyfour::WebDriver,
                __browser_test_context: &'__context #context_alias #type_generics,
            ) -> ::std::pin::Pin<::std::boxed::Box<
                dyn ::std::future::Future<Output = #crate_path::__private::DynamicResult>
                    + ::std::marker::Send
                    + '__future
            >>
            where
                '__self: '__future,
                '__driver: '__future,
                '__context: '__future,
                Self: '__future,
            {
                #boxed_body
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
    use assertr::prelude::*;
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
                "async fn test(driver: &thirtyfour::WebDriver) -> Result<(), Report> { Ok(()) }",
                "a single argument is the run's context, not the session's driver",
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
                "async fn result() -> Result<(), Report> { Ok(()) }",
                "shadowing the prelude's `Result`",
            ),
            (
                "async fn copy() -> Result<(), Report> { Ok(()) }",
                "shadowing the prelude's `Copy`",
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
            assert_that!(error.to_string()).contains(message);
        }
    }

    #[test]
    fn reserved_names_are_sorted() {
        assert_that!(super::RESERVED_NAMES.is_sorted()).is_true();
    }
}
