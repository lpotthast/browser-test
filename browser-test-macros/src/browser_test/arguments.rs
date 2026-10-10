//! The arguments of `#[browser_test(...)]`.

use manyhow::error_message;
use syn::{
    Ident, LitStr, Token,
    parse::{Parse, ParseStream},
};

/// The arguments of `#[browser_test(...)]`, all optional.
#[derive(Default)]
pub(crate) struct Arguments {
    /// `name = "..."`: the test's name, replacing the module-qualified function name.
    pub(super) name: Option<LitStr>,
}

impl Parse for Arguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "name" {
                return Err(error_message!(key, "expected `name = \"...\"`").into());
            }
            if args.name.is_some() {
                return Err(error_message!(key, "duplicate `name` argument").into());
            }
            input.parse::<Token![=]>()?;
            let name: LitStr = input.parse()?;
            if name.value().is_empty() {
                return Err(error_message!(name, "test names must not be empty").into());
            }
            args.name = Some(name);
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::Arguments;

    #[test]
    fn name_arguments_accept_strings_and_reject_invalid_options() {
        assert!(syn::parse_str::<Arguments>("").unwrap().name.is_none());
        let args = syn::parse_str::<Arguments>("name = \"keyboard::events\",").unwrap();
        assert_eq!(args.name.unwrap().value(), "keyboard::events");
        for (source, message) in [
            ("path = \"/keyboard\"", "expected `name = \"...\"`"),
            (
                "name = \"one\", name = \"two\"",
                "duplicate `name` argument",
            ),
            ("name = 42", "expected string literal"),
            ("name = \"\"", "test names must not be empty"),
        ] {
            assert_eq!(
                syn::parse_str::<Arguments>(source)
                    .err()
                    .unwrap()
                    .to_string(),
                message
            );
        }
    }
}
