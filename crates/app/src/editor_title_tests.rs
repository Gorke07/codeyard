//! Small tests for editor helpers (in a separate file to protect editor.rs's line budget).

#[test]
fn title_case_handles_apostrophes_and_symbols() {
    assert_eq!(crate::editor::title_case("hello wORLD it's"), "Hello World It's");
    assert_eq!(crate::editor::title_case("snake_case-word 2nd"), "Snake_Case-Word 2nd");
    assert_eq!(crate::editor::title_case(""), "");
}

#[test]
fn hover_expression_follows_member_chains() {
    let at = |line: &str, col: usize| crate::editor::expression_in_line(&line.chars().collect::<Vec<_>>(), col);
    let l = "    total += p.pos.x + q->next->val;";
    assert_eq!(at(l, 4).as_deref(), Some("total"));
    assert_eq!(at(l, 16).as_deref(), Some("p.pos.x"), "a word in the middle of a chain yields the whole chain");
    assert_eq!(at(l, 24).as_deref(), Some("q->next->val"));
    assert_eq!(at(l, 27).as_deref(), Some("q->next->val"));
    assert_eq!(at("x = 42;", 5), None, "an expression, not a number");
    assert_eq!(at("a - b", 2), None, "whitespace / operator");
    assert_eq!(at("end.", 3).as_deref(), Some("end"), "the trailing dot is dropped");
}
