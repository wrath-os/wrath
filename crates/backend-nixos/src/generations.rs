//! The generations of the system profile.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub number: u32,
    /// When it was created, as `nix-env` prints it.
    pub created: String,
    pub current: bool,
    /// The hash of the model it was built from, when `distro` built it.
    pub model_hash: Option<String>,
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hash = self
            .model_hash
            .as_deref()
            .map_or("not built by distro", |hash| &hash[..hash.len().min(12)]);
        write!(f, "{:>4}  {}  model {hash}", self.number, self.created)?;
        if self.current {
            write!(f, "  (current)")?;
        }
        Ok(())
    }
}

/// Parses `nix-env --list-generations`, lines such as `   2   2026-11-06 10:31:07   (current)`.
pub fn parse_generations(listing: &str) -> Result<Vec<Generation>, String> {
    listing
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let (Some(number), Some(date), Some(time)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("unexpected generation line: {line}"));
            };
            let number = number
                .parse()
                .map_err(|_| format!("unexpected generation line: {line}"))?;
            Ok(Generation {
                number,
                created: format!("{date} {time}"),
                current: fields.next() == Some("(current)"),
                model_hash: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generations_and_marks_the_current_one() {
        let generations = parse_generations(
            "   1   2026-11-06 10:00:00   \n   2   2026-11-06 10:31:07   (current)\n",
        )
        .unwrap();

        assert_eq!(
            generations,
            [
                Generation {
                    number: 1,
                    created: "2026-11-06 10:00:00".into(),
                    current: false,
                    model_hash: None,
                },
                Generation {
                    number: 2,
                    created: "2026-11-06 10:31:07".into(),
                    current: true,
                    model_hash: None,
                },
            ]
        );
    }

    #[test]
    fn rejects_lines_it_does_not_understand() {
        assert!(parse_generations("garbage\n").is_err());
    }

    #[test]
    fn shows_a_short_hash_and_the_current_marker() {
        let generation = Generation {
            number: 2,
            created: "2026-11-06 10:31:07".into(),
            current: true,
            model_hash: Some("0123456789abcdef0123".into()),
        };

        assert_eq!(
            generation.to_string(),
            "   2  2026-11-06 10:31:07  model 0123456789ab  (current)"
        );
    }
}
