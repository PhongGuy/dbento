//! Tolerant script splitter over `pg_query::scan`: lexes only, so a statement the parser
//! rejects still becomes its own piece and the server reports the error.

use std::ops::Range;

use pg_query::protobuf::Token;

/// One statement-sized slice of a script.
pub(crate) struct Piece {
    pub(crate) range: Range<usize>,
    pub(crate) blank: bool,
}

/// Contiguous pieces covering all of `sql`; `blank` when a piece holds only comments,
/// whitespace and its `;`. A lexical error makes the whole text one piece.
pub(crate) fn split(sql: &str) -> Vec<Piece> {
    let Ok(scanned) = pg_query::scan(sql) else {
        return vec![Piece {
            range: 0..sql.len(),
            blank: sql.trim().is_empty(),
        }];
    };
    // Comments never count as code and never move the nesting depth.
    let code: Vec<(Token, usize)> = scanned
        .tokens
        .iter()
        .map(|t| {
            (
                Token::try_from(t.token).unwrap_or(Token::Nul),
                t.end as usize,
            )
        })
        .filter(|(k, _)| !matches!(k, Token::SqlComment | Token::CComment))
        .collect();

    // A `;` ends a piece only at depth 0. BEGIN ATOMIC bodies open a level, and CASE...END
    // inside one nests so its END is not taken for the body's.
    let (mut pieces, mut start, mut depth, mut blank) = (Vec::new(), 0usize, 0u32, true);
    for (i, &(token, end)) in code.iter().enumerate() {
        match token {
            Token::Ascii59 if depth == 0 => {
                pieces.push(Piece {
                    range: start..end,
                    blank,
                });
                start = end;
                blank = true;
                continue;
            }
            Token::BeginP if code.get(i + 1).is_some_and(|n| n.0 == Token::Atomic) => depth += 1,
            Token::Case if depth > 0 => depth += 1,
            Token::EndP if depth > 0 => depth -= 1,
            _ => {}
        }
        blank = false;
    }
    if start < sql.len() {
        pieces.push(Piece {
            range: start..sql.len(),
            blank,
        });
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(sql: &str) -> Vec<(&str, bool)> {
        let out: Vec<_> = split(sql)
            .into_iter()
            .map(|p| (&sql[p.range], p.blank))
            .collect();
        assert_eq!(
            out.iter().map(|p| p.0).collect::<String>(),
            sql,
            "text was dropped"
        );
        out
    }

    #[test]
    fn splits_on_top_level_semicolons_only() {
        assert_eq!(
            pieces("SELECT 1; SELEC x; SELECT 2"),
            vec![
                ("SELECT 1;", false),
                (" SELEC x;", false),
                (" SELECT 2", false)
            ]
        );
        assert_eq!(
            pieces("SELECT ';' ; SELECT \"a;b\" FROM t"),
            vec![("SELECT ';' ;", false), (" SELECT \"a;b\" FROM t", false)]
        );
        assert_eq!(
            pieces("DO $$ BEGIN PERFORM 1; END $$; SELECT 4"),
            vec![
                ("DO $$ BEGIN PERFORM 1; END $$;", false),
                (" SELECT 4", false)
            ]
        );
        assert_eq!(
            pieces("BEGIN; SELECT 6; END;"),
            vec![("BEGIN;", false), (" SELECT 6;", false), (" END;", false)]
        );
    }

    #[test]
    fn begin_atomic_bodies_stay_whole_even_with_a_comment_inside_the_keyword_pair() {
        let sql = "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN /* c */ ATOMIC SELECT 1; SELECT CASE WHEN true THEN 1 END; END; SELECT 3";
        assert_eq!(
            pieces(sql),
            vec![(&sql[..sql.len() - 9], false), (" SELECT 3", false)]
        );
    }

    #[test]
    fn comment_only_and_empty_pieces_are_blank() {
        assert_eq!(
            pieces("SELECT 5; -- trailing ; comment\n/* c; */"),
            vec![
                ("SELECT 5;", false),
                (" -- trailing ; comment\n/* c; */", true)
            ]
        );
        assert_eq!(pieces(";;"), vec![(";", true), (";", true)]);
        assert_eq!(pieces("  "), vec![("  ", true)]);
        assert!(pieces("").is_empty());
    }

    #[test]
    fn offsets_are_bytes() {
        assert_eq!(
            pieces("SELECT 'æøå'; SELECT 2"),
            vec![("SELECT 'æøå';", false), (" SELECT 2", false)]
        );
    }

    #[test]
    fn a_lexical_error_makes_the_script_one_piece() {
        assert_eq!(
            pieces("SELECT 1; SELECT 'oops"),
            vec![("SELECT 1; SELECT 'oops", false)]
        );
    }
}
