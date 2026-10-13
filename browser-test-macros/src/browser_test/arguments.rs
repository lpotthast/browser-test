//! The arguments of `#[browser_test(...)]`.

use manyhow::error_message;
use syn::{
    Expr, Ident, LitStr, Token,
    parse::{Parse, ParseStream},
};

/// The arguments of `#[browser_test(...)]`, all optional.
#[derive(Default)]
pub(crate) struct Arguments {
    /// `name = "..."`: the test's name, replacing the module-qualified function name.
    pub(super) name: Option<LitStr>,
    /// `time_limit = <expr>`: the test's time limit, a `std::time::Duration`.
    pub(super) time_limit: Option<Expr>,
}

impl Parse for Arguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let duplicate = match key.to_string().as_str() {
                "name" => args.name.is_some(),
                "time_limit" => args.time_limit.is_some(),
                _ => {
                    return Err(error_message!(
                        key,
                        "expected `name = \"...\"` or `time_limit = <Duration>`"
                    )
                    .into());
                }
            };
            if duplicate {
                return Err(error_message!(key, "duplicate `{key}` argument").into());
            }
            input.parse::<Token![=]>()?;
            if key == "name" {
                let name: LitStr = input.parse()?;
                if name.value().is_empty() {
                    return Err(error_message!(name, "test names must not be empty").into());
                }
                args.name = Some(name);
            } else {
                args.time_limit = Some(input.parse()?);
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use quote::ToTokens;

    use super::Arguments;

    #[test]
    fn time_limit_arguments_accept_expressions() {
        let args = syn::parse_str::<Arguments>(
            "time_limit = std::time::Duration::from_secs(5), name = \"slow\"",
        )
        .unwrap();
        assert_eq!(
            args.time_limit.unwrap().to_token_stream().to_string(),
            "std :: time :: Duration :: from_secs (5)"
        );
        assert_eq!(args.name.unwrap().value(), "slow");
        assert!(
            syn::parse_str::<Arguments>("")
                .unwrap()
                .time_limit
                .is_none()
        );
    }

    #[test]
    fn name_arguments_accept_strings_and_reject_invalid_options() {
        assert!(syn::parse_str::<Arguments>("").unwrap().name.is_none());
        let args = syn::parse_str::<Arguments>("name = \"keyboard::events\",").unwrap();
        assert_eq!(args.name.unwrap().value(), "keyboard::events");
        for (source, message) in [
            (
                "path = \"/keyboard\"",
                "expected `name = \"...\"` or `time_limit = <Duration>`",
            ),
            (
                "name = \"one\", name = \"two\"",
                "duplicate `name` argument",
            ),
            ("name = 42", "expected string literal"),
            ("name = \"\"", "test names must not be empty"),
            (
                "time_limit = Duration::from_secs(1), time_limit = Duration::from_secs(2)",
                "duplicate `time_limit` argument",
            ),
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
