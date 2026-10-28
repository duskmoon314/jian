use std::ops::Range;

use anyhow::{Context, Result, anyhow, bail};
use markdown::{Constructs, ParseOptions, mdast::Node, to_mdast};

/// A source-first Markdown document. Only the active source range is edited;
/// every other byte remains untouched.
#[derive(Debug)]
pub struct SourceDocument {
    source: String,
    blocks: Vec<SourceBlock>,
}

#[derive(Clone, Debug)]
struct SourceBlock {
    range: Range<usize>,
}

impl SourceDocument {
    pub fn new(source: String) -> Result<Self> {
        let blocks = parse_blocks(&source)?;
        Ok(Self { source, blocks })
    }

    /// Keep arbitrary source as one opaque range when a full-source edit is
    /// temporarily unparsable. Saving never depends on a successful reparse.
    pub fn opaque(source: String) -> Self {
        let range = 0..source.len();
        Self {
            source,
            blocks: vec![SourceBlock { range }],
        }
    }

    pub fn text(&self) -> &str {
        &self.source
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn block_source(&self, index: usize) -> Option<&str> {
        self.blocks
            .get(index)
            .map(|block| &self.source[block.range.clone()])
    }

    /// Replace one active range and shift later ranges without reparsing.
    /// This permits the active editor to temporarily contain several blocks.
    pub fn replace_block(&mut self, index: usize, replacement: &str) -> Result<()> {
        let Some(block) = self.blocks.get(index).cloned() else {
            bail!("invalid active Markdown block {index}");
        };
        let old_len = block.range.len();
        self.source.replace_range(block.range.clone(), replacement);
        self.blocks[index].range.end = block.range.start + replacement.len();

        let delta = replacement.len() as i128 - old_len as i128;
        for later in &mut self.blocks[index + 1..] {
            later.range.start = shift(later.range.start, delta)?;
            later.range.end = shift(later.range.end, delta)?;
        }
        Ok(())
    }

    /// Recompute block ranges after leaving an active editor.
    pub fn reparse(&mut self) -> Result<()> {
        self.blocks = parse_blocks(&self.source)?;
        Ok(())
    }
}

fn shift(offset: usize, delta: i128) -> Result<usize> {
    let shifted = offset as i128 + delta;
    usize::try_from(shifted).context("source range underflow")
}

fn parse_blocks(source: &str) -> Result<Vec<SourceBlock>> {
    let options = ParseOptions {
        constructs: Constructs {
            frontmatter: true,
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    };
    let ast = to_mdast(source, &options).map_err(|error| anyhow!(error.to_string()))?;
    let Node::Root(root) = ast else {
        bail!("Markdown parser did not return a root node");
    };

    let blocks = root
        .children
        .into_iter()
        .map(|node| {
            let position = node
                .position()
                .context("top-level Markdown block has no source position")?;
            let range = position.start.offset..position.end.offset;
            if range.start > range.end
                || range.end > source.len()
                || !source.is_char_boundary(range.start)
                || !source.is_char_boundary(range.end)
            {
                bail!("invalid Markdown block source range {range:?}");
            }
            Ok(SourceBlock { range })
        })
        .collect::<Result<Vec<_>>>()?;
    if blocks.is_empty() {
        Ok(vec![SourceBlock {
            range: 0..source.len(),
        }])
    } else {
        Ok(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_one_block_preserves_every_other_byte() {
        let original = "# Title\n\nold **bold**\n\n* item\n\n";
        let mut document = SourceDocument::new(original.into()).unwrap();
        document.replace_block(1, "new **bold**").unwrap();
        assert_eq!(document.text(), "# Title\n\nnew **bold**\n\n* item\n\n");
        assert_eq!(document.block_source(2), Some("* item\n"));
    }

    #[test]
    fn active_block_can_become_multiple_blocks() {
        let mut document = SourceDocument::new("first\n\nlast\n".into()).unwrap();
        document.replace_block(0, "first\n\n inserted").unwrap();
        assert_eq!(document.block_source(1), Some("last"));
        document.reparse().unwrap();
        assert_eq!(document.block_count(), 3);
        assert_eq!(document.text(), "first\n\n inserted\n\nlast\n");
    }

    #[test]
    fn multibyte_and_crlf_ranges_remain_valid() {
        let mut document =
            SourceDocument::new("# 标题\r\n\r\n旧内容\r\n\r\n尾段\r\n".into()).unwrap();
        document.replace_block(1, "中文🙂").unwrap();
        assert_eq!(document.block_source(2), Some("尾段"));
        assert_eq!(document.text(), "# 标题\r\n\r\n中文🙂\r\n\r\n尾段\r\n");
        document.reparse().unwrap();
        assert_eq!(document.block_count(), 3);
    }

    #[test]
    fn frontmatter_is_one_opaque_block() {
        let document = SourceDocument::new("---\ntitle: Test\n---\n\nBody\n".into()).unwrap();
        assert_eq!(document.block_count(), 2);
        assert_eq!(document.block_source(0), Some("---\ntitle: Test\n---"));
    }

    #[test]
    fn empty_document_has_one_editable_range() {
        let mut document = SourceDocument::new(String::new()).unwrap();
        assert_eq!(document.block_count(), 1);
        assert_eq!(document.block_source(0), Some(""));
        document.replace_block(0, "# Started").unwrap();
        assert_eq!(document.text(), "# Started");
    }
}
