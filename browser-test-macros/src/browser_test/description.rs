//! `BrowserTest::description`, taken from the test's doc comments.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Expr, ExprLit, Lit, Meta};

/// The body of `BrowserTest::description`: the doc comments in `docs`, as written, without the
/// conventional space after each comment marker.
pub(super) fn expand(docs: &[Attribute]) -> TokenStream {
    let values: Vec<_> = docs
        .iter()
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(meta) => Some(&meta.value),
            _ => None,
        })
        .collect();
    if values.is_empty() {
        return quote!(::std::option::Option::None);
    }
    let literals: Option<Vec<_>> = values.iter().copied().map(string_literal).collect();
    if let Some(literals) = literals {
        // Ordinary doc comments are normalized at expansion time.
        let text = normalize(&literals.join("\n"));
        quote!(::std::option::Option::Some(::std::borrow::Cow::Borrowed(#text)))
    } else {
        // Expressions such as `#[doc = include_str!("test.md")]` expand in the caller. The
        // generated code repeats `normalize`.
        quote! {
            ::std::option::Option::Some(::std::borrow::Cow::Owned(
                [#(#values),*].join("\n").lines()
                    .map(|line| line.strip_prefix(' ').unwrap_or(line))
                    .collect::<::std::vec::Vec<_>>().join("\n")
                    .trim_matches('\n').to_owned()
            ))
        }
    }
}

/// The value of a plain doc comment, `None` for a computed one.
fn string_literal(value: &Expr) -> Option<String> {
    match value {
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => Some(lit.value()),
        _ => None,
    }
}

/// Strips one leading space from every line, and the empty lines around the text.
fn normalize(text: &str) -> String {
    text.lines()
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_strips_the_comment_space_and_surrounding_empty_lines() {
        assert_eq!(
            normalize("\n Summary.\n\n     Indented.\n\n"),
            "Summary.\n\n    Indented."
        );
    }
}
