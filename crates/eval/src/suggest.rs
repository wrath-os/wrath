//! "Did you mean" suggestions for misspelled names.

/// A `help:` note naming the closest candidate, if one is close enough to be a likely typo.
pub(crate) fn did_you_mean<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a String>,
) -> Option<String> {
    closest(name, candidates.into_iter().map(String::as_str))
        .map(|candidate| format!("help: did you mean `{candidate}`?"))
}

pub(crate) fn closest<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .map(|candidate| (distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min()
        .map(|(_, candidate)| candidate)
}

/// Levenshtein distance, counting a swap of two neighbouring characters as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![(0..=b.len()).collect::<Vec<_>>(); a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        let names = ["hostname", "timezone", "port"];

        assert_eq!(closest("hostnme", names), Some("hostname"));
        assert_eq!(closest("prot", names), Some("port"));
        assert_eq!(closest("enable", names), None);
        assert_eq!(distance("ab", "ba"), 1);
        assert_eq!(distance("", "abc"), 3);
    }
}
