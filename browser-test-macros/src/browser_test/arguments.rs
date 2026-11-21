//! The arguments of `#[browser_test(...)]`.

use manyhow::error_message;
use syn::{
    Expr, Ident, LitStr, Path, Token, bracketed,
    ext::IdentExt as _,
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
    /// `crate = <path>`: the path to `browser-test`, replacing the one found in `Cargo.toml`.
    pub(super) crate_path: Option<Path>,
}

impl Parse for Arguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        while !input.is_empty() {
            // `crate` is a keyword.
            let key = Ident::parse_any(input)?;
            let duplicate = match key.to_string().as_str() {
                "name" => args.name.is_some(),
                "tags" => args.tags.is_some(),
                "time_limit" => args.time_limit.is_some(),
                "crate" => args.crate_path.is_some(),
                _ => {
                    return Err(error_message!(
                        key,
                        "expected `name = \"...\"`, `tags = [\"...\"]`, `time_limit = <Duration>` \
                         or `crate = <path>`"
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
                    } else if seen.contains(&value) {
                        "duplicate tag"
                    } else {
                        seen.push(value);
                        continue;
                    };
                    return Err(error_message!(tag, "{problem}").into());
                }
                args.tags = Some(tags.into_iter().collect());
            } else if key == "time_limit" {
                args.time_limit = Some(input.parse()?);
            } else {
                args.crate_path = Some(input.parse()?);
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
        let args = syn::parse_str::<Arguments>("tags = [\"button,focus\", \" spaced \"]").unwrap();
        let tags: Vec<_> = args.tags.unwrap().iter().map(LitStr::value).collect();
        assert_that!(tags).contains_exactly(["button,focus", " spaced "]);
        let args = syn::parse_str::<Arguments>("tags = []").unwrap();
        assert_that!(args.tags).some().is_empty();
    }

    #[test]
    fn crate_arguments_accept_paths() {
        assert_that!(syn::parse_str::<Arguments>("").unwrap().crate_path).is_none();
        let args = syn::parse_str::<Arguments>("crate = crate::reexports::browser_test").unwrap();
        assert_that!(args.crate_path.unwrap().to_token_stream().to_string())
            .is_equal_to("crate :: reexports :: browser_test");
    }

    #[test]
    fn name_arguments_accept_strings_and_reject_invalid_options() {
        assert_that!(syn::parse_str::<Arguments>("").unwrap().name).is_none();
        let args = syn::parse_str::<Arguments>("name = \"keyboard::events\",").unwrap();
        assert_that!(args.name.unwrap().value()).is_equal_to("keyboard::events");
        for (source, message) in [
            (
                "path = \"/keyboard\"",
                "expected `name = \"...\"`, `tags = [\"...\"]`, `time_limit = <Duration>` or \
                 `crate = <path>`",
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
            ("tags = [\"button\", \"button\"]", "duplicate tag"),
            ("tags = [], tags = []", "duplicate `tags` argument"),
            (
                "time_limit = Duration::from_secs(1), time_limit = Duration::from_secs(2)",
                "duplicate `time_limit` argument",
            ),
            ("crate = \"browser_test\"", "expected identifier"),
            ("crate = a, crate = b", "duplicate `crate` argument"),
        ] {
            assert_that!(syn::parse_str::<Arguments>(source).err())
                .some()
                .has_display_value(message);
        }
    }
}
