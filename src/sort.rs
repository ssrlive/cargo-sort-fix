use std::{cmp::Ordering, collections::BTreeMap, iter::FromIterator};

use toml_edit::{Array, Decor, DocumentMut, Item, RawString, Table, Value};

/// Leading string for combining keys such as
/// `[target.'cfg(target_os="linux")'.dependencies]` in Cargo.toml files.
const TARGET: &str = "target";

/// Stores the paths of target tables in a BTreeMap, the data structure looks like:
/// ```plain
/// target_tables: {
///     "build-dependencies": [],
///     "dependencies": [
///         ["target", "cfg(any(target_os = \"macos\", target_os = \"freebsd\"))", "dependencies"],
///         ["target", "cfg(target_os = \"windows\")", "dependencies"],
///         ["target", "cfg(unix)", "dependencies"]
///     ],
///     "dev-dependencies": [
///         ["target", "cfg(target_os = \"windows\")", "dev-dependencies"],
///         ["target", "cfg(unix)", "dev-dependencies"]
///     ]
/// }
/// ```
type TargetTablePaths = BTreeMap<String, Vec<Vec<String>>>;

/// Each `Matcher` field when matched to a heading or key token
/// will be matched with `.contains()`.
#[derive(Debug)]
pub(crate) struct Matcher<'a> {
    /// Toml headings with braces `[heading]`.
    pub heading: &'a [&'a str],
    /// Toml heading with braces `[heading]` and the key
    /// of the array to sort.
    pub heading_key: &'a [(&'a str, &'a str)],
}

pub(crate) const MATCHER: Matcher<'_> = Matcher {
    heading: &["dependencies", "dev-dependencies", "build-dependencies"],
    heading_key: &[
        ("workspace", "members"),
        ("workspace", "exclude"),
        ("workspace", "dependencies"),
        ("workspace", "dev-dependencies"),
        ("workspace", "build-dependencies"),
    ],
};

/// A state machine to track collection of headings.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Heading {
    /// After collecting heading segments we recurse into another table.
    Next(Vec<String>),
    /// We have found a completed heading.
    ///
    /// The the heading we are processing has key value pairs.
    Complete(Vec<String>),
}

/// Returns a sorted toml `DocumentMut`.
pub(crate) fn sort_toml(input: &str, matcher: Matcher<'_>, group: bool, ordering: &[String]) -> DocumentMut {
    let mut ordering = ordering.to_owned();
    let mut toml = input.parse::<DocumentMut>().unwrap();
    // This takes care of `[workspace] members = [...]`
    for (heading, key) in matcher.heading_key {
        // Since this `&mut toml[&heading]` is like
        // `SomeMap.entry(key).or_insert(Item::None)` we only want to do it if we
        // know the heading is there already
        if toml.as_table().contains_key(heading)
            && let Item::Table(table) = &mut toml[heading]
            && table.contains_key(key)
        {
            match &mut table[key] {
                Item::Value(Value::Array(arr)) => {
                    sort_array(arr);
                }
                Item::Table(table) => {
                    sort_table(table, group);
                }
                _ => {}
            }
        }
    }

    let mut first_table = None;
    let mut heading_order: BTreeMap<_, Vec<Heading>> = BTreeMap::new();
    for (idx, (head, item)) in toml.as_table_mut().iter_mut().enumerate() {
        let mut target_tables: TargetTablePaths = BTreeMap::new();
        let item_key = head.get();
        if item_key == TARGET
            && let Some(table) = item.as_table()
        {
            for &key in matcher.heading {
                let mut path = vec![item_key];
                let mut deps_tables = vec![];
                nested_tables_with_key(table, &mut path, key, &mut deps_tables);
                let deps_tables = deps_tables
                    .iter()
                    .map(|p| p.iter().map(|&s| s.to_owned()).collect::<Vec<_>>())
                    .collect::<Vec<_>>();
                target_tables.entry(key.to_owned()).or_default().extend(deps_tables);
            }
        }

        if !matcher.heading.contains(&item_key) && target_tables.is_empty() {
            if !ordering.contains(&head.to_owned()) && !ordering.is_empty() {
                ordering.push(head.to_owned());
            }
            continue;
        }
        match item {
            Item::Table(table) => {
                if first_table.is_none() {
                    // The root table is always index 0 which we ignore so add 1
                    first_table = Some(idx + 1);
                }
                let key = item_key.to_owned();
                let headings = heading_order.entry((idx, key.clone())).or_default();
                // Push a `Heading::Complete` here incase the tables are ordered
                // [heading.segs]
                // [heading]
                // It will just be ignored if not the case
                headings.push(Heading::Complete(vec![key]));

                gather_headings(table, headings, 1);
                headings.sort();
                sort_table(table, group);
                sort_nested_table(table, &target_tables);
            }
            Item::None => continue,
            _ => {}
        }
    }

    if ordering.is_empty() {
        sort_lexicographical(first_table, &heading_order, &mut toml);
    } else {
        sort_by_ordering(&ordering, &heading_order, &mut toml);
    }

    toml
}

fn nested_tables_with_key<'a>(table: &'a Table, path: &mut Vec<&'a str>, key_name: &str, result: &mut Vec<Vec<&'a str>>) {
    for (key, item) in table.iter() {
        path.push(key);
        if let Item::Table(inner) = item {
            if key == key_name && inner.position().is_some() {
                result.push(path.clone());
            }
            nested_tables_with_key(inner, path, key_name, result);
        }
        path.pop();
    }
}

fn sort_array(arr: &mut Array) {
    let mut all_strings = true;
    let trailing = arr.trailing().clone();
    let trailing_comma = arr.trailing_comma();

    let mut arr_copy = arr.iter().cloned().collect::<Vec<_>>();
    arr_copy.sort_by(|a, b| match (a, b) {
        (Value::String(a), Value::String(b)) => a.value().cmp(b.value()),
        _ => {
            all_strings = false;
            Ordering::Equal
        }
    });
    if all_strings {
        *arr = Array::from_iter(arr_copy);
    }

    arr.set_trailing(trailing);
    arr.set_trailing_comma(trailing_comma);
}

fn sort_table(table: &mut Table, group: bool) {
    if group {
        sort_by_group(table);
    } else {
        table.sort_values();
    }
}

fn sort_nested_table(table: &mut Table, target_tables: &TargetTablePaths) {
    // The `table` name must be `target`
    for paths in target_tables.values() {
        for path in paths {
            if path.len() > 1 {
                sort_table_by_path(table, &path[1..]);
            }
        }
    }
}

fn sort_table_by_path(table: &mut Table, path: &[String]) {
    let Some(first) = path.first() else {
        table.sort_values();
        return;
    };
    if let Some(Item::Table(inner_table)) = table.get_mut(first) {
        sort_table_by_path(inner_table, &path[1..]);
    }
}

fn gather_headings(table: &Table, keys: &mut Vec<Heading>, depth: usize) {
    if table.is_empty() && !table.is_implicit() {
        let next = match keys.pop().unwrap() {
            Heading::Next(segs) => Heading::Complete(segs),
            comp => comp,
        };
        keys.push(next);
    }
    for (head, item) in table.iter() {
        match item {
            Item::Value(_) => {
                if keys.last().is_some_and(|h| matches!(h, Heading::Complete(_))) {
                    continue;
                }
                let next = match keys.pop().unwrap() {
                    Heading::Next(segs) => Heading::Complete(segs),
                    _complete => unreachable!("the above if check prevents this"),
                };
                keys.push(next);
                continue;
            }
            Item::Table(table) => {
                let next = match keys.pop().unwrap() {
                    Heading::Next(mut segs) => {
                        segs.push(head.into());
                        Heading::Next(segs)
                    }
                    // This happens when
                    //
                    // [heading]       // transitioning from here to
                    // [heading.segs]  // here
                    Heading::Complete(segs) => {
                        let take = depth.max(1);
                        let mut next = segs[..take].to_vec();
                        next.push(head.into());
                        keys.push(Heading::Complete(segs));
                        Heading::Next(next)
                    }
                };
                keys.push(next);
                gather_headings(table, keys, depth + 1);
            }
            Item::ArrayOfTables(_arr) => unreachable!("no [[heading]] are sorted"),
            Item::None => unreachable!("an empty table will not be sorted"),
        }
    }
}

fn sort_by_group(table: &mut Table) {
    let table_clone = table.clone();
    table.clear();

    let mut groups = BTreeMap::new();
    let mut group_decor = BTreeMap::default();

    let mut curr = 0;
    for (idx, (k, _)) in table_clone.iter().enumerate() {
        let (k, v) = table_clone.get_key_value(k).unwrap();

        // If the item is a dotted table, grab the decor of the first item of the table
        // instead.
        let decor = if let Some(first_in_dotted) = v.as_table().filter(|t| t.is_dotted()).and_then(|t| t.key(t.iter().next()?.0)) {
            first_in_dotted.leaf_decor()
        } else {
            k.leaf_decor()
        };

        let blank_lines = decor
            .prefix()
            .and_then(RawString::as_str)
            .unwrap_or("")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .count();

        if blank_lines > 0 {
            let decor = k.leaf_decor().clone();
            let k = k.clone().with_leaf_decor(Decor::default());

            groups.entry(idx).or_insert_with(|| vec![(k, v)]);
            group_decor.insert(idx, decor);
            curr = idx;
        } else {
            groups.entry(curr).or_default().push((k.clone(), v));
        }
    }

    for (idx, mut group) in groups {
        group.sort_by(|a, b| a.0.cmp(&b.0));
        let group_decor = group_decor.remove(&idx);

        for (idx, (mut k, v)) in group.into_iter().enumerate() {
            if idx == 0
                && let Some(group_decor) = group_decor.clone()
            {
                k = k.with_leaf_decor(group_decor);
            }

            table.insert_formatted(&k, v.clone());
        }
    }
}

fn sort_lexicographical(first_table: Option<usize>, heading_order: &BTreeMap<(usize, String), Vec<Heading>>, toml: &mut DocumentMut) {
    // Since the root table is always index 0 we add one
    let first_table_idx = first_table.unwrap_or_default() + 1;
    for (idx, heading) in heading_order.iter().flat_map(|(_, segs)| segs).enumerate() {
        if let Heading::Complete(segs) = heading {
            let mut nested = 0;
            let mut table = Some(toml.as_table_mut());
            for seg in segs {
                nested += 1;
                table = table.and_then(|t| t[seg].as_table_mut());
            }
            // Do not reorder the unsegmented tables
            if nested > 1
                && let Some(table) = table
            {
                table.set_position((first_table_idx + idx) as isize);
            }
        }
    }
}

fn sort_by_ordering(ordering: &[String], heading_order: &BTreeMap<(usize, String), Vec<Heading>>, toml: &mut DocumentMut) {
    let mut idx = 0;
    for heading in ordering {
        let mut matches: Vec<(&(usize, String), &Vec<Heading>)> = heading_order
            .iter()
            .filter(|((_s, key), headings)| {
                key == heading
                    || headings.iter().any(|h| {
                        if let Heading::Complete(segs) = h {
                            segs.iter().any(|seg| seg == heading)
                        } else {
                            false
                        }
                    })
            })
            .collect();

        /// Use `heading` as the split point, and divide `segs` into two parts:
        /// - Traverse left (backward) to the start (including `heading`)
        /// - Traverse right (forward) to the end (after `heading`)
        ///
        /// Then join both parts into a dot-separated string, for example:
        /// `[target.'cfg(windows)'.dependencies.windows-sys]` will be
        /// `[dependencies.'cfg(windows)'.target.windows-sys]`
        fn join_segs_around_heading(segs: &[String], heading: &str) -> Option<String> {
            if let Some(pos) = segs.iter().position(|seg| seg == heading) {
                let mut left: Vec<_> = segs[..=pos].iter().rev().cloned().collect();
                let right: Vec<_> = if pos + 1 < segs.len() {
                    segs[pos + 1..].to_vec()
                } else {
                    Vec::new()
                };
                left.extend(right);
                return Some(left.join("."));
            }
            None
        }

        fn extract_heading_segments(headings: &[Heading], heading: &str) -> String {
            headings
                .iter()
                .filter_map(|h| {
                    if let Heading::Complete(segs) = h {
                        return join_segs_around_heading(segs, heading);
                    }
                    None
                })
                .max_by_key(|s| s.len())
                .unwrap_or_default()
        }

        matches.sort_by(|((_, a_key), a_headings), ((_, b_key), b_headings)| {
            let a1_longest = extract_heading_segments(a_headings, heading);
            let b1_longest = extract_heading_segments(b_headings, heading);
            let ord = a1_longest.cmp(&b1_longest);
            if ord == Ordering::Equal { a_key.cmp(b_key) } else { ord }
        });

        if !matches.is_empty() {
            for &((_, key), to_sort_headings) in &matches {
                let mut to_sort_headings = to_sort_headings
                    .iter()
                    .filter(|h| {
                        if let Heading::Complete(segs) = h {
                            if key == TARGET {
                                // Get rid of the items that do not contain the heading
                                return segs.iter().any(|seg| seg == heading);
                            } else {
                                return true;
                            }
                        }
                        false
                    })
                    .collect::<Vec<_>>();
                to_sort_headings.sort_by_key(|h| {
                    if let Heading::Complete(segs) = h {
                        if key == TARGET {
                            join_segs_around_heading(segs, heading).unwrap_or_default()
                        } else {
                            segs.to_vec().join(".")
                        }
                    } else {
                        String::new()
                    }
                });
                for h in to_sort_headings {
                    if let Heading::Complete(segs) = h {
                        let mut table = Some(toml.as_table_mut());
                        for seg in segs {
                            table = table.and_then(|t| t[seg].as_table_mut());
                        }
                        // Do not reorder the unsegmented tables
                        if let Some(table) = table {
                            table.set_position(idx);
                            idx += 1;
                        }
                    }
                }
            }
        } else if let Some(tab) = toml.as_table_mut()[heading].as_table_mut() {
            tab.set_position(idx);
            idx += 1;
            walk_tables_set_position(tab, &mut idx);
        } else if let Some(arrtab) = toml.as_table_mut()[heading].as_array_of_tables_mut() {
            for tab in arrtab.iter_mut() {
                tab.set_position(idx);
                idx += 1;
                walk_tables_set_position(tab, &mut idx);
            }
        }
    }
}

fn walk_tables_set_position(table: &mut Table, idx: &mut isize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(tab) => {
                tab.set_position(*idx);
                *idx += 1;
                walk_tables_set_position(tab, idx);
            }
            Item::ArrayOfTables(arr) => {
                for tab in arr.iter_mut() {
                    tab.set_position(*idx);
                    *idx += 1;
                    walk_tables_set_position(tab, idx);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use super::MATCHER;
    use crate::test_utils::assert_eq;

    #[test]
    fn toml_edit_check() {
        let input = fs::read_to_string("examp/workspace.toml").unwrap();
        let expected = fs::read_to_string("examp/workspace.sorted.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, false, &[]);
        assert_eq(expected, sorted);
    }

    #[test]
    fn toml_combined_key_check() {
        let input = fs::read_to_string("examp/tun.toml").unwrap();
        let expected = fs::read_to_string("examp/tun.sorted.toml").unwrap();
        let o = crate::fmt::DEF_TABLE_ORDER;
        let o = o.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
        let sorted = super::sort_toml(&input, MATCHER, false, &o);

        assert_eq(expected, sorted);
    }

    #[test]
    fn toml_workspace_deps_edit_check() {
        let input = fs::read_to_string("examp/workspace_deps.toml").unwrap();
        let expected = fs::read_to_string("examp/workspace_deps.sorted.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, false, &[]);
        assert_eq(expected, sorted);
    }

    #[test]
    fn grouped_check() {
        let input = fs::read_to_string("examp/ruma.toml").unwrap();
        let expected = fs::read_to_string("examp/ruma.sorted.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(expected, sorted);
    }

    #[test]
    fn sort_correct() {
        let input = fs::read_to_string("examp/right.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(input, sorted);
    }

    #[test]
    fn sort_comments() {
        let input = fs::read_to_string("examp/comments.toml").unwrap();
        let expected = fs::read_to_string("examp/comments.sorted.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(expected, sorted);
    }

    #[test]
    fn sort_tables() {
        let input = fs::read_to_string("examp/fend.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_ne!(input, sorted.to_string());
        // println!("{}", sorted.to_string());
    }

    #[test]
    fn sort_devfirst() {
        let input = fs::read_to_string("examp/reorder.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(input, sorted);

        let input = fs::read_to_string("examp/noreorder.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(input, sorted);
    }

    #[test]
    fn issue_104() {
        let input = fs::read_to_string("regressions/104.toml").unwrap();
        let sorted = super::sort_toml(&input, MATCHER, true, &[]);
        assert_eq(input, sorted);
    }

    #[test]
    fn reorder() {
        let input = fs::read_to_string("examp/clippy.toml").unwrap();
        let sorted = super::sort_toml(
            &input,
            MATCHER,
            true,
            &[
                "package".to_owned(),
                "features".to_owned(),
                "dependencies".to_owned(),
                "build-dependencies".to_owned(),
                "dev-dependencies".to_owned(),
            ],
        );
        assert_ne!(input, sorted.to_string());
    }
}
