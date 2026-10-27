//! The arguments of `#[browser_test(...)]`.

use manyhow::error_message;
use syn::{
    Expr, Ident, LitStr, Token, bracketed,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

/// The arguments of `#[browser_test(...)]`, all optional.
#[derive(Default)]
pub(crate) struct Arguments {
    /// `name = "..."`: the test's name, replacing the module-qualified function name.
    pub(super) name: Option<LitStr>,
    /// `tags = ["...", ...]`: the test's tags.
    pub(super) tags: Option<Vec<LitStr>>,
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
                "tags" => args.tags.is_some(),
                "time_limit" => args.time_limit.is_some(),
                _ => {
                    return Err(error_message!(
                        key,
                        "expected `name = \"...\"`, `tags = [\"...\"]` or `time_limit = <Duration>`"
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
            } else if key == "tags" {
                let content;
                bracketed!(content in input);
                let tags = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
                let mut seen = Vec::new();
                for tag in &tags {
                    let value = tag.value();
                    let problem = if value.is_empty() {
                        "tags must not be empty"
                    } else if value.contains(',') || value.trim() != value {
                        // `BROWSER_TEST_TAG` splits at commas and trims, so it couldn't select it.
                        "tags must not contain commas or start or end with whitespace"
                    } else if seen.contains(&value) {
                        "duplicate tag"
                    } else {
                        seen.push(value);
                        continue;
                    };
                    return Err(error_message!(tag, "{problem}").into());
                }
                args.tags = Some(tags.into_iter().collect());
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
    use assertr::prelude::*;
    use quote::ToTokens;

    use syn::LitStr;

    use super::Arguments;

    #[test]
    fn time_limit_arguments_accept_expressions() {
        let args = syn::parse_str::<Arguments>(
            "time_limit = std::time::Duration::from_secs(5), name = \"slow\"",
        )
        .unwrap();
        assert_that!(args.time_limit.unwrap().to_token_stream().to_string())
            .is_equal_to("std :: time :: Duration :: from_secs (5)");
        assert_that!(args.name.unwrap().value()).is_equal_to("slow");
        assert_that!(syn::parse_str::<Arguments>("").unwrap().time_limit).is_none();
    }

    #[test]
    fn tags_arguments_accept_lists_of_strings() {
        assert_that!(syn::parse_str::<Arguments>("").unwrap().tags).is_none();
        let args = syn::parse_str::<Arguments>("tags = [\"button\", \"keyboard\",]").unwrap();
        let tags: Vec<_> = args.tags.unwrap().iter().map(LitStr::value).collect();
        assert_that!(tags).contains_exactly(["button", "keyboard"]);
        let args = syn::parse_str::<Arguments>("tags = []").unwrap();
        assert_that!(args.tags).some().is_empty();
    }

    #[test]
    fn name_arguments_accept_strings_and_reject_invalid_options() {
        assert_that!(syn::parse_str::<Arguments>("").unwrap().name).is_none();
        let args = syn::parse_str::<Arguments>("name = \"keyboard::events\",").unwrap();
        assert_that!(args.name.unwrap().value()).is_equal_to("keyboard::events");
        for (source, message) in [
            (
                "path = \"/keyboard\"",
                "expected `name = \"...\"`, `tags = [\"...\"]` or `time_limit = <Duration>`",
            ),
            (
                "name = \"one\", name = \"two\"",
                "duplicate `name` argument",
            ),
            ("name = 42", "expected string literal"),
            ("name = \"\"", "test names must not be empty"),
            ("tags = \"button\"", "expected square brackets"),
            ("tags = [\"button\", 42]", "expected string literal"),
            ("tags = [\"button\", \"\"]", "tags must not be empty"),
            (
                "tags = [\"button,focus\"]",
                "tags must not contain commas or start or end with whitespace",
            ),
            (
                "tags = [\" button\"]",
                "tags must not contain commas or start or end with whitespace",
            ),
            ("tags = [\"button\", \"button\"]", "duplicate tag"),
            ("tags = [], tags = []", "duplicate `tags` argument"),
            (
                "time_limit = Duration::from_secs(1), time_limit = Duration::from_secs(2)",
                "duplicate `time_limit` argument",
            ),
        ] {
            assert_that!(syn::parse_str::<Arguments>(source).err())
                .some()
                .has_display_value(message);
        }
    }
}
