//! Destructive-statement guard.
//!
//! Scans the significant words of a SQL string while skipping string literals,
//! quoted identifiers, and comments, so `select 'drop'` or `delete_log` do not
//! trip the guard. Dollar-quoted PostgreSQL bodies are not understood; those
//! are a documented limitation.

const DESTRUCTIVE: [&str; 3] = ["drop", "truncate", "delete"];

/// Keywords refused on `read_only` connections. Superset of DESTRUCTIVE:
/// every data-definition or data-modifying verb counts as a write.
const WRITES: [&str; 9] = [
    "insert", "update", "delete", "drop", "alter", "truncate", "create", "grant", "revoke",
];

fn keyword_in(sql: &str, list: &[&str]) -> Option<String> {
    significant_words(sql)
        .into_iter()
        .find(|word| list.contains(&word.to_ascii_lowercase().as_str()))
        .map(|word| word.to_ascii_lowercase())
}

/// Returns the first destructive keyword found, lower-cased.
pub fn destructive_keyword(sql: &str) -> Option<String> {
    keyword_in(sql, &DESTRUCTIVE)
}

/// Returns the first write keyword found, lower-cased. Used for `read_only`
/// connections; SELECT/EXPLAIN/SHOW/PRAGMA queries find nothing here.
pub fn write_keyword(sql: &str) -> Option<String> {
    keyword_in(sql, &WRITES)
}

fn significant_words(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            byte if is_word_byte(byte) => {
                let start = i;
                while i < bytes.len() && is_word_byte(bytes[i]) {
                    i += 1;
                }
                words.push(sql[start..i].to_string());
            }
            _ => i += 1,
        }
    }
    words
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_reads_and_benign_words() {
        assert_eq!(destructive_keyword("select * from users"), None);
        assert_eq!(destructive_keyword("delete_log"), None);
        assert_eq!(destructive_keyword("select 'delete from x'"), None);
        assert_eq!(destructive_keyword("select \"drop\" from `truncate`"), None);
        assert_eq!(destructive_keyword("-- delete\nselect 1"), None);
        assert_eq!(destructive_keyword("/* drop table x */ select 1"), None);
        assert_eq!(destructive_keyword("select updated_at from t"), None);
    }

    #[test]
    fn blocks_destructive_statements() {
        assert_eq!(
            destructive_keyword("DELETE FROM users").as_deref(),
            Some("delete")
        );
        assert_eq!(
            destructive_keyword("  drop table users").as_deref(),
            Some("drop")
        );
        assert_eq!(
            destructive_keyword("TRUNCATE TABLE users").as_deref(),
            Some("truncate")
        );
        assert_eq!(
            destructive_keyword("update t set x = 1; delete from t;").as_deref(),
            Some("delete")
        );
    }

    #[test]
    fn write_keyword_blocks_every_write_verb() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "update t set x = 1",
            "delete from t",
            "drop table t",
            "alter table t add column c int",
            "truncate table t",
            "create index i on t (c)",
            "grant select on t to u",
            "revoke select on t from u",
        ] {
            assert!(write_keyword(sql).is_some(), "{sql}");
        }
    }

    #[test]
    fn write_keyword_allows_reads() {
        for sql in [
            "select * from t",
            "explain select * from t",
            "show tables",
            "pragma table_info(t)",
            "select create_counter from t",
            "select 'insert' as word",
        ] {
            assert_eq!(write_keyword(sql), None, "{sql}");
        }
    }
}
