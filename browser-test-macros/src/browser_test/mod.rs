//! The expansion of `#[browser_test]`: a unit struct implementing `BrowserTest` by running the
//! annotated function.

mod arguments;
mod description;
mod signature;

use arguments::Arguments;
use heck::ToUpperCamelCase;
use manyhow::{ErrorMessage, bail, ensure, error_message};
use proc_macro2::{Ident, TokenStream};
use quote::{ToTokens as _, format_ident, quote, quote_spanned};
use signature::{Inputs, TestSignature};
use syn::{
    Attribute, ItemFn, Meta, Token, Visibility, ext::IdentExt, punctuated::Punctuated,
    spanned::Spanned as _,
};

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

/// The expansion of `#[browser_test]` on `item`, with `args`. On an error, `dummy` holds the
/// function and its struct, so that neither the struct's uses nor the function's body report
/// errors of their own.
pub(crate) fn expand(
    args: TokenStream,
    item: TokenStream,
    dummy: &mut TokenStream,
) -> manyhow::Result<TokenStream> {
    let function = match syn::parse2::<ItemFn>(item.clone()) {
        Ok(function) => function,
        Err(error) => {
            *dummy = item;
            bail!(
                error.span(),
                "`#[browser_test]` must be applied to an `async fn`"
            );
        }
    };
    let args = syn::parse2::<Arguments>(args);
    let crate_path = match &args {
        Ok(Arguments {
            crate_path: Some(path),
            ..
        }) => Ok(path.to_token_stream()),
        _ => crate_path(),
    };
    *dummy = expand_dummy(function.clone(), crate_path.as_ref().ok());
    expand_test(args?, &crate_path?, function)
}

/// The test of `function`, with `args`.
fn expand_test(
    args: Arguments,
    crate_path: &TokenStream,
    mut function: ItemFn,
) -> manyhow::Result<TokenStream> {
    let TestSignature {
        result,
        context,
        generics,
        inputs,
        input_spans,
    } = TestSignature::parse(&function.sig)?;
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
    let attributes = StructAttributes::take(&mut function);
    let description = description::expand(&attributes.docs);
    let cfgs = &attributes.cfgs;
    let test_struct = attributes.test_struct(&name);
    let body = &function.sig.ident;
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    let call_args = match inputs {
        Inputs::None => &[][..],
        Inputs::Context => &["__browser_test_context"][..],
        Inputs::DriverAndContext => &["__browser_test_driver", "__browser_test_context"][..],
    };
    // Spanned at the arguments' types, where an argument of another type is reported.
    let call_args = call_args
        .iter()
        .zip(input_spans)
        .map(|(arg, span)| Ident::new(arg, span));
    // Spanned at the return type, where a wrong one is reported.
    let into_dynamic = quote_spanned! {result.span()=>
        <#result as #crate_path::__private::TestResult>::into_dynamic
    };
    // Spanned at the function's name, where a future that is not `Send` is reported.
    let boxed_body = quote_spanned! {body.span()=>
        ::std::boxed::Box::pin(#crate_path::__private::run_body(#body(#(#call_args),*), #into_dynamic))
    };
    Ok(quote! {
        #test_struct

        #function

        // `run` returns the function's future without an `async` block of its own, only wrapped in
        // browser-test's `run_body`, so that no frame of generated code is on the stack while the
        // test runs, and failure reports show the function as the test's outermost frame.
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

            fn run<'__browser_test>(
                &'__browser_test self,
                __browser_test_driver: &'__browser_test #crate_path::thirtyfour::WebDriver,
                __browser_test_context: &'__browser_test #context,
            ) -> #crate_path::TestFuture<'__browser_test> {
                #boxed_body
            }
        }
    })
}

/// What the attribute expands to besides an error: `function` and its struct, implementing
/// `BrowserTest` for every context if `crate_path` is known.
fn expand_dummy(mut function: ItemFn, crate_path: Option<&TokenStream>) -> TokenStream {
    let name = struct_name(&function.sig.ident, &function.sig.ident.unraw().to_string()).ok();
    let attributes = StructAttributes::take(&mut function);
    // A free function with a receiver is an error of its own.
    let function = function.sig.receiver().is_none().then(|| {
        quote! {
            #[allow(dead_code)]
            #function
        }
    });
    let Some(name) = name else {
        return function.into_token_stream();
    };
    let test_struct = attributes.test_struct(&name);
    let cfgs = &attributes.cfgs;
    let implementation = crate_path.map(|crate_path| {
        quote! {
            #(#cfgs)*
            impl<__BrowserTestContext> #crate_path::BrowserTest<__BrowserTestContext> for #name
            where
                __BrowserTestContext: ?::core::marker::Sized + ::core::marker::Sync,
            {
                fn run<'__browser_test>(
                    &'__browser_test self,
                    _: &'__browser_test #crate_path::thirtyfour::WebDriver,
                    _: &'__browser_test __BrowserTestContext,
                ) -> #crate_path::TestFuture<'__browser_test> {
                    ::core::unreachable!()
                }
            }
        }
    });
    quote! {
        #[allow(dead_code)]
        #test_struct

        #function

        #implementation
    }
}

/// The attributes of a test's function that belong to its struct.
struct StructAttributes {
    /// The doc comments, which document the test.
    docs: Vec<Attribute>,
    /// The `#[cfg]`s, which must guard every generated item, including the implementation.
    cfgs: Vec<Attribute>,
    /// The lint attributes meant for the struct (see [`take_lints`]).
    lints: Vec<Attribute>,
    /// The function's visibility.
    visibility: Visibility,
}

impl StructAttributes {
    /// Takes the struct's attributes from `function`. The function keeps every other attribute:
    /// one meant for the function, e.g. `#[expect(...)]` or `#[tracing::instrument]`, could fail on
    /// the struct. It keeps its name, which failure reports and `#[tracing::instrument]` show, and
    /// the struct gets its visibility.
    fn take(function: &mut ItemFn) -> Self {
        let docs = function
            .attrs
            .extract_if(.., |attr| attr.path().is_ident("doc"))
            .collect();
        let cfgs = function
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg"))
            .cloned()
            .collect();
        let lints = take_lints(&mut function.attrs);
        let visibility = std::mem::replace(&mut function.vis, Visibility::Inherited);
        Self {
            docs,
            cfgs,
            lints,
            visibility,
        }
    }

    /// The declaration of the test's struct, `name`.
    fn test_struct(&self, name: &Ident) -> TokenStream {
        let Self {
            docs,
            cfgs,
            lints,
            visibility,
        } = self;
        quote! {
            #(#docs)*
            #(#cfgs)*
            #(#lints)*
            #[derive(
                ::std::fmt::Debug,
                ::std::clone::Clone,
                ::std::marker::Copy,
                ::std::default::Default,
            )]
            #visibility struct #name;
        }
    }
}

/// Takes the struct's lint attributes from `attrs`: copies of the `#[allow]`s, which are harmless
/// where nothing triggers them, and the `missing_docs` lints of the `#[expect]`s, which only the
/// struct, public unlike the function, can fulfill.
fn take_lints(attrs: &mut Vec<Attribute>) -> Vec<Attribute> {
    let mut lints = Vec::new();
    attrs.retain_mut(|attr| {
        if attr.path().is_ident("allow") {
            lints.push(attr.clone());
            return true;
        }
        if !attr.path().is_ident("expect") {
            return true;
        }
        let Meta::List(list) = &attr.meta else {
            return true;
        };
        let Ok(entries) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        else {
            return true;
        };
        let (reason, entries): (Vec<_>, Vec<_>) = entries.into_iter().partition(
            |entry| matches!(entry, Meta::NameValue(entry) if entry.path.is_ident("reason")),
        );
        let (missing_docs, others): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| matches!(entry, Meta::Path(path) if path.is_ident("missing_docs")));
        if missing_docs.is_empty() {
            return true;
        }
        // The attribute, with the lints `entries` and its reason.
        let relisted = |entries: &[Meta]| {
            let mut attr = attr.clone();
            if let Meta::List(list) = &mut attr.meta {
                list.tokens = quote!(#(#entries,)* #(#reason)*);
            }
            attr
        };
        lints.push(relisted(&missing_docs));
        if others.is_empty() {
            return false;
        }
        *attr = relisted(&others);
        true
    });
    lints
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

    use quote::ToTokens as _;
    use syn::{Attribute, parse_quote};

    use super::{Arguments, crate_path, expand_test, take_lints};

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
                "async fn test(context: &str, driver: &WebDriver) -> Result<(), Report> { Ok(()) }",
                "the second argument is the run's context, not the session's driver",
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
            let error = expand_test(Arguments::default(), &crate_path().unwrap(), function)
                .unwrap_err()
                .into_token_stream();
            assert_that!(error.to_string()).contains(message);
        }
    }

    #[test]
    fn struct_lints_are_allows_and_expected_missing_docs() {
        let mut attrs: Vec<Attribute> = vec![
            parse_quote!(#[allow(dead_code)]),
            parse_quote!(#[expect(missing_docs, unused_variables, reason = "generated")]),
            parse_quote!(#[expect(missing_docs)]),
            parse_quote!(#[expect(unused_mut)]),
            parse_quote!(#[inline]),
        ];
        let lints = take_lints(&mut attrs);
        let tokens = |attrs: &[Attribute]| {
            attrs
                .iter()
                .map(|attr| attr.to_token_stream().to_string())
                .collect::<Vec<_>>()
        };
        assert_that!(tokens(&lints)).contains_exactly([
            "# [allow (dead_code)]",
            "# [expect (missing_docs , reason = \"generated\")]",
            "# [expect (missing_docs ,)]",
        ]);
        assert_that!(tokens(&attrs)).contains_exactly([
            "# [allow (dead_code)]",
            "# [expect (unused_variables , reason = \"generated\")]",
            "# [expect (unused_mut)]",
            "# [inline]",
        ]);
    }

    #[test]
    fn reserved_names_are_sorted() {
        assert_that!(super::RESERVED_NAMES.is_sorted()).is_true();
    }
}
