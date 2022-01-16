use std::fmt::{Display, Write};

pub struct IndentedText<'a>(pub usize, pub &'a str);

impl<'a> Display for IndentedText<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (n, line) in self.1.lines().enumerate() {
            if n > 0 {
                f.write_char('\n')?;
            }
            for _ in 0..self.0 {
                f.write_char(' ')?;
            }
            f.write_str(line)?;
        }

        Ok(())
    }
}
