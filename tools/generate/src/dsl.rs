use crate::{
    GenerateError, GenerateLimits, content_limit, object_limit, syntax_error, token_limit,
    topology_error, unsupported,
};

pub(crate) struct OnePageSpec {
    pub(crate) media_box: [i64; 4],
    pub(crate) content: Vec<u8>,
}

pub(crate) fn parse(source: &[u8], limits: GenerateLimits) -> Result<OnePageSpec, GenerateError> {
    if let Err(error) = std::str::from_utf8(source) {
        return Err(syntax_error(Some(error.valid_up_to())));
    }

    let mut parser = Parser::new(source, limits);
    parser.expect_identifier("document")?;
    parser.expect_symbol(b'(')?;
    parser.expect_identifier("version")?;
    parser.expect_symbol(b':')?;
    let (version, version_offset) = parser.expect_string(parser.lexer.limits.max_source_bytes())?;
    if version != b"1.7" {
        return Err(unsupported(Some(version_offset)));
    }
    parser.expect_symbol(b')')?;
    parser.expect_symbol(b'{')?;

    parser.parse_catalog()?;
    parser.parse_pages()?;
    let media_box = parser.parse_page()?;
    let content = parser.parse_stream()?;
    parser.parse_xref()?;

    parser.expect_symbol(b'}')?;
    parser.expect_end()?;
    if media_box[2] <= media_box[0] || media_box[3] <= media_box[1] {
        return Err(topology_error(None));
    }
    Ok(OnePageSpec { media_box, content })
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    objects: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a [u8], limits: GenerateLimits) -> Self {
        Self {
            lexer: Lexer::new(source, limits),
            objects: 0,
        }
    }

    fn parse_catalog(&mut self) -> Result<(), GenerateError> {
        self.parse_object_start(1, "catalog")?;
        self.expect_symbol(b'(')?;
        self.expect_identifier("pages")?;
        self.expect_symbol(b':')?;
        self.expect_reference(2)?;
        self.expect_symbol(b')')?;
        self.expect_symbol(b';')
    }

    fn parse_pages(&mut self) -> Result<(), GenerateError> {
        self.parse_object_start(2, "pages")?;
        self.expect_symbol(b'(')?;
        self.expect_identifier("kids")?;
        self.expect_symbol(b':')?;
        self.expect_symbol(b'[')?;
        self.expect_reference(3)?;
        self.expect_symbol(b']')?;
        self.expect_symbol(b',')?;
        self.expect_identifier("count")?;
        self.expect_symbol(b':')?;
        self.expect_exact_integer(1)?;
        self.expect_symbol(b')')?;
        self.expect_symbol(b';')
    }

    fn parse_page(&mut self) -> Result<[i64; 4], GenerateError> {
        self.parse_object_start(3, "page")?;
        self.expect_symbol(b'(')?;
        self.expect_identifier("media_box")?;
        self.expect_symbol(b':')?;
        self.expect_symbol(b'[')?;
        let mut media_box = [0_i64; 4];
        for (index, value) in media_box.iter_mut().enumerate() {
            *value = self.expect_integer()?;
            if index != 3 {
                self.expect_symbol(b',')?;
            }
        }
        self.expect_symbol(b']')?;
        self.expect_symbol(b',')?;
        self.expect_identifier("resources")?;
        self.expect_symbol(b':')?;
        self.expect_symbol(b'{')?;
        self.expect_symbol(b'}')?;
        self.expect_symbol(b',')?;
        self.expect_identifier("contents")?;
        self.expect_symbol(b':')?;
        self.expect_reference(4)?;
        self.expect_symbol(b')')?;
        self.expect_symbol(b';')?;
        Ok(media_box)
    }

    fn parse_stream(&mut self) -> Result<Vec<u8>, GenerateError> {
        let start = self.expect_identifier("stream")?;
        self.expect_symbol(b'(')?;
        self.expect_exact_integer(4)?;
        let closing = self.next()?;
        match closing.kind {
            TokenKind::Symbol(b')') => {}
            TokenKind::Symbol(b',') | TokenKind::Identifier(_) => {
                return Err(unsupported(Some(closing.offset)));
            }
            _ => return Err(syntax_error(Some(closing.offset))),
        }
        self.register_object(start)?;
        self.expect_symbol(b'{')?;
        let (content, offset) = self.expect_string(self.lexer.limits.max_content_bytes())?;
        if content.len() > self.lexer.limits.max_content_bytes() {
            return Err(content_limit(Some(offset)));
        }
        self.expect_symbol(b'}')?;
        Ok(content)
    }

    fn parse_xref(&mut self) -> Result<(), GenerateError> {
        let token = self.next()?;
        match token.kind {
            TokenKind::Identifier("xref") => {}
            TokenKind::Identifier(_) => return Err(unsupported(Some(token.offset))),
            _ => return Err(syntax_error(Some(token.offset))),
        }
        self.expect_symbol(b'(')?;
        self.expect_identifier("kind")?;
        self.expect_symbol(b':')?;
        let kind = self.next()?;
        match kind.kind {
            TokenKind::Identifier("table") => {}
            TokenKind::Identifier(_) => return Err(unsupported(Some(kind.offset))),
            _ => return Err(syntax_error(Some(kind.offset))),
        }
        self.expect_symbol(b')')?;
        self.expect_symbol(b';')
    }

    fn parse_object_start(
        &mut self,
        expected_number: i64,
        expected_kind: &str,
    ) -> Result<(), GenerateError> {
        let start = self.expect_identifier("object")?;
        self.expect_symbol(b'(')?;
        self.expect_exact_integer(expected_number)?;
        self.expect_symbol(b')')?;
        self.expect_symbol(b'=')?;
        let kind = self.next()?;
        match kind.kind {
            TokenKind::Identifier(value) if value == expected_kind => {}
            TokenKind::Identifier(_) => return Err(unsupported(Some(kind.offset))),
            _ => return Err(syntax_error(Some(kind.offset))),
        }
        self.register_object(start)
    }

    fn register_object(&mut self, offset: usize) -> Result<(), GenerateError> {
        self.objects = self
            .objects
            .checked_add(1)
            .ok_or_else(|| object_limit(Some(offset)))?;
        if self.objects > self.lexer.limits.max_objects() {
            return Err(object_limit(Some(offset)));
        }
        Ok(())
    }

    fn expect_reference(&mut self, expected: i64) -> Result<(), GenerateError> {
        self.expect_identifier("ref")?;
        self.expect_symbol(b'(')?;
        self.expect_exact_integer(expected)?;
        self.expect_symbol(b')')
    }

    fn expect_exact_integer(&mut self, expected: i64) -> Result<(), GenerateError> {
        let token = self.next()?;
        match token.kind {
            TokenKind::Integer(value) if value == expected => Ok(()),
            TokenKind::Integer(_) => Err(topology_error(Some(token.offset))),
            _ => Err(syntax_error(Some(token.offset))),
        }
    }

    fn expect_integer(&mut self) -> Result<i64, GenerateError> {
        let token = self.next()?;
        match token.kind {
            TokenKind::Integer(value) => Ok(value),
            _ => Err(syntax_error(Some(token.offset))),
        }
    }

    fn expect_identifier(&mut self, expected: &str) -> Result<usize, GenerateError> {
        let token = self.next()?;
        match token.kind {
            TokenKind::Identifier(value) if value == expected => Ok(token.offset),
            TokenKind::Identifier(_) => Err(syntax_error(Some(token.offset))),
            _ => Err(syntax_error(Some(token.offset))),
        }
    }

    fn expect_string(
        &mut self,
        max_decoded_bytes: usize,
    ) -> Result<(Vec<u8>, usize), GenerateError> {
        let token = self.lexer.next_with_string_limit(max_decoded_bytes)?;
        match token.kind {
            TokenKind::String(value) => Ok((value, token.offset)),
            _ => Err(syntax_error(Some(token.offset))),
        }
    }

    fn expect_symbol(&mut self, expected: u8) -> Result<(), GenerateError> {
        let token = self.next()?;
        match token.kind {
            TokenKind::Symbol(value) if value == expected => Ok(()),
            _ => Err(syntax_error(Some(token.offset))),
        }
    }

    fn expect_end(&mut self) -> Result<(), GenerateError> {
        let token = self.next()?;
        if matches!(token.kind, TokenKind::End) {
            Ok(())
        } else {
            Err(syntax_error(Some(token.offset)))
        }
    }

    fn next(&mut self) -> Result<Token<'a>, GenerateError> {
        self.lexer
            .next_with_string_limit(self.lexer.limits.max_source_bytes())
    }
}

struct Lexer<'a> {
    source: &'a [u8],
    cursor: usize,
    tokens: usize,
    limits: GenerateLimits,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a [u8], limits: GenerateLimits) -> Self {
        Self {
            source,
            cursor: 0,
            tokens: 0,
            limits,
        }
    }

    fn next_with_string_limit(
        &mut self,
        max_decoded_bytes: usize,
    ) -> Result<Token<'a>, GenerateError> {
        self.skip_trivia();
        let offset = self.cursor;
        let Some(byte) = self.source.get(self.cursor).copied() else {
            return Ok(Token {
                kind: TokenKind::End,
                offset,
            });
        };

        let next_tokens = self
            .tokens
            .checked_add(1)
            .ok_or_else(|| token_limit(Some(offset)))?;
        if next_tokens > self.limits.max_tokens() {
            return Err(token_limit(Some(offset)));
        }
        self.tokens = next_tokens;

        let kind = if byte.is_ascii_alphabetic() || byte == b'_' {
            self.lex_identifier()
        } else if byte.is_ascii_digit() || byte == b'-' {
            self.lex_integer()?
        } else if byte == b'"' {
            TokenKind::String(self.lex_string(max_decoded_bytes)?)
        } else if b"(){}[],:;=".contains(&byte) {
            self.cursor += 1;
            TokenKind::Symbol(byte)
        } else {
            return Err(syntax_error(Some(offset)));
        };

        Ok(Token { kind, offset })
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .source
                .get(self.cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.cursor += 1;
            }
            if self.source.get(self.cursor) != Some(&b'#') {
                return;
            }
            while self
                .source
                .get(self.cursor)
                .is_some_and(|byte| *byte != b'\n')
            {
                self.cursor += 1;
            }
        }
    }

    fn lex_identifier(&mut self) -> TokenKind<'a> {
        let start = self.cursor;
        self.cursor += 1;
        while self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
        {
            self.cursor += 1;
        }
        let value = std::str::from_utf8(&self.source[start..self.cursor])
            .expect("identifier bytes are restricted to ASCII");
        TokenKind::Identifier(value)
    }

    fn lex_integer(&mut self) -> Result<TokenKind<'a>, GenerateError> {
        let start = self.cursor;
        let negative = self.source.get(self.cursor) == Some(&b'-');
        if negative {
            self.cursor += 1;
        }
        let digit_start = self.cursor;
        let mut magnitude = 0_u64;
        while let Some(byte) = self.source.get(self.cursor).copied() {
            if !byte.is_ascii_digit() {
                break;
            }
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or_else(|| syntax_error(Some(start)))?;
            self.cursor += 1;
        }
        if self.cursor == digit_start {
            return Err(syntax_error(Some(start)));
        }
        let value = if negative {
            if magnitude == (i64::MAX as u64) + 1 {
                i64::MIN
            } else {
                let value = i64::try_from(magnitude).map_err(|_| syntax_error(Some(start)))?;
                -value
            }
        } else {
            i64::try_from(magnitude).map_err(|_| syntax_error(Some(start)))?
        };
        Ok(TokenKind::Integer(value))
    }

    fn lex_string(&mut self, max_decoded_bytes: usize) -> Result<Vec<u8>, GenerateError> {
        let start = self.cursor;
        self.cursor += 1;
        let mut output = Vec::new();
        loop {
            let Some(byte) = self.source.get(self.cursor).copied() else {
                return Err(syntax_error(Some(start)));
            };
            self.cursor += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let Some(escaped) = self.source.get(self.cursor).copied() else {
                        return Err(syntax_error(Some(start)));
                    };
                    self.cursor += 1;
                    let decoded = match escaped {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'\\' => b'\\',
                        b'"' => b'"',
                        b'x' => {
                            let high = self.hex_digit(start)?;
                            let low = self.hex_digit(start)?;
                            (high << 4) | low
                        }
                        _ => return Err(syntax_error(Some(self.cursor - 1))),
                    };
                    self.push_string_byte(&mut output, decoded, start, max_decoded_bytes)?;
                }
                0x00..=0x1f | 0x7f => return Err(syntax_error(Some(self.cursor - 1))),
                value => self.push_string_byte(&mut output, value, start, max_decoded_bytes)?,
            }
        }
    }

    fn hex_digit(&mut self, string_start: usize) -> Result<u8, GenerateError> {
        let Some(byte) = self.source.get(self.cursor).copied() else {
            return Err(syntax_error(Some(string_start)));
        };
        self.cursor += 1;
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(syntax_error(Some(self.cursor - 1))),
        }
    }

    fn push_string_byte(
        &self,
        output: &mut Vec<u8>,
        byte: u8,
        offset: usize,
        max_decoded_bytes: usize,
    ) -> Result<(), GenerateError> {
        if output.len() >= max_decoded_bytes {
            return Err(content_limit(Some(offset)));
        }
        output
            .try_reserve(1)
            .map_err(|_| content_limit(Some(offset)))?;
        output.push(byte);
        Ok(())
    }
}

struct Token<'a> {
    kind: TokenKind<'a>,
    offset: usize,
}

enum TokenKind<'a> {
    Identifier(&'a str),
    Integer(i64),
    String(Vec<u8>),
    Symbol(u8),
    End,
}
