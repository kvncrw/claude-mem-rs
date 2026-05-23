pub fn render_footer(tokens_saved: usize) -> String {
    format!("</claude-mem-context>\n<!-- ~{tokens_saved}t saved -->\n")
}
