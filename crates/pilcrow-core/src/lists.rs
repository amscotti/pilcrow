//! List markers read from the bytes of one source line.
//!
//! Enter continues a list only when the marker is literally there. The block
//! scanner's kind is a rendering decision; inventing a marker would create
//! Markdown the file does not contain.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub indent: usize,
    pub marker_end: usize,
    pub ordered: bool,
    pub number: usize,
    pub punct: u8,
    pub bullet: u8,
    pub task: bool,
    pub checked: bool,
}

fn space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// Parse the list marker at the head of a line. `marker_end` includes the
/// separator's trailing space and, for a task, the task box.
pub fn parse(line: &[u8]) -> Option<Item> {
    let mut i = 0;
    while i < line.len() && space(line[i]) {
        i += 1;
    }
    if i >= line.len() {
        return None;
    }

    let mut item = Item {
        indent: i,
        marker_end: 0,
        ordered: false,
        number: 0,
        punct: b'.',
        bullet: b'-',
        task: false,
        checked: false,
    };
    if matches!(line[i], b'-' | b'*' | b'+') {
        if i + 1 >= line.len() || !space(line[i + 1]) {
            return None;
        }
        item.bullet = line[i];
        item.marker_end = i + 2;
    } else {
        let mut j = i;
        while j < line.len() && line[j].is_ascii_digit() {
            j += 1;
        }
        if j == i || j >= line.len() || !matches!(line[j], b'.' | b')') {
            return None;
        }
        if j + 1 >= line.len() || !space(line[j + 1]) {
            return None;
        }
        item.ordered = true;
        item.number = std::str::from_utf8(&line[i..j]).ok()?.parse().ok()?;
        if item.number == usize::MAX {
            return None;
        }
        item.punct = line[j];
        item.marker_end = j + 2;
    }

    let at = item.marker_end;
    if at + 2 < line.len()
        && line[at] == b'['
        && matches!(line[at + 1], b' ' | b'x' | b'X')
        && line[at + 2] == b']'
        && (at + 3 >= line.len() || space(line[at + 3]))
    {
        item.task = true;
        item.checked = matches!(line[at + 1], b'x' | b'X');
        item.marker_end += 3;
        if item.marker_end < line.len() && space(line[item.marker_end]) {
            item.marker_end += 1;
        }
    }
    if item.marker_end < line.len() && space(line[item.marker_end]) {
        item.marker_end += 1;
    }
    Some(item)
}

/// The marker for the next item: same indentation and punctuation, the next
/// number, and an empty task box.
pub fn next_marker(line: &str, item: Item) -> String {
    let mut marker = String::with_capacity(item.indent + 16);
    marker.push_str(&line[..item.indent.min(line.len())]);
    if item.ordered {
        marker.push_str(&(item.number.saturating_add(1)).to_string());
        marker.push(item.punct as char);
    } else {
        marker.push(item.bullet as char);
    }
    marker.push(' ');
    if item.task {
        marker.push_str("[ ] ");
    }
    marker
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bullets_ordered_tasks_and_nesting() {
        let item = parse(b"- [ ] task").expect("task");
        assert!(item.task);
        assert!(!item.checked);
        assert_eq!(item.marker_end, 6);
        assert_eq!(item.indent, 0);

        let item = parse(b"  2) item").expect("ordered");
        assert!(item.ordered);
        assert_eq!(item.number, 2);
        assert_eq!(item.punct, b')');
        assert_eq!(item.indent, 2);
        assert_eq!(item.marker_end, 5);

        assert!(parse(b"plain").is_none());
        assert!(parse(b"-no space").is_none());
    }

    #[test]
    fn next_marker_continues_numbering_and_resets_tasks() {
        let line = "  - [x] done";
        let item = parse(line.as_bytes()).expect("task");
        assert_eq!(next_marker(line, item), "  - [ ] ");

        let line = "3. item";
        let item = parse(line.as_bytes()).expect("ordered");
        assert_eq!(next_marker(line, item), "4. ");
    }
}
