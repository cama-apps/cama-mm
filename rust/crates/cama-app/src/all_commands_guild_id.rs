//! Static guild-scope safety checks for command modules.
//!
//! These checks are migration tooling, not Discord runtime dispatch. They port
//! the contract in `tests/test_all_commands_guild_id.py`: within each Python
//! function, a direct `guild_id` call argument must not precede the first
//! simple `guild_id = ...` assignment. Command sources are embedded at compile
//! time so deleted or renamed modules fail the Rust build as well.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSource {
    pub filename: &'static str,
    pub source: &'static str,
}

macro_rules! command_source {
    ($filename:literal) => {
        CommandSource {
            filename: $filename,
            source: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../commands/",
                $filename
            )),
        }
    };
}

pub const ALL_COMMAND_SOURCES: &[CommandSource] = &[
    command_source!("__init__.py"),
    command_source!("admin.py"),
    command_source!("advstats.py"),
    command_source!("ask.py"),
    command_source!("betting.py"),
    command_source!("blame_luke.py"),
    command_source!("checks.py"),
    command_source!("dig.py"),
    command_source!("dota_info.py"),
    command_source!("draft.py"),
    command_source!("duel.py"),
    command_source!("enrichment.py"),
    command_source!("herogrid.py"),
    command_source!("info.py"),
    command_source!("lobby.py"),
    command_source!("mafia.py"),
    command_source!("mana.py"),
    command_source!("match.py"),
    command_source!("pet.py"),
    command_source!("player_trivia.py"),
    command_source!("predictions.py"),
    command_source!("profile.py"),
    command_source!("rating_analysis.py"),
    command_source!("registration.py"),
    command_source!("reminders.py"),
    command_source!("scout.py"),
    command_source!("shop.py"),
    command_source!("survey.py"),
    command_source!("tax.py"),
    command_source!("trivia.py"),
    command_source!("wrapped.py"),
];

pub const MAINTAINED_COMMAND_MODULES: &[&str] = &[
    "commands.admin",
    "commands.advstats",
    "commands.betting",
    "commands.draft",
    "commands.enrichment",
    "commands.herogrid",
    "commands.info",
    "commands.lobby",
    "commands.match",
    "commands.predictions",
    "commands.profile",
    "commands.rating_analysis",
    "commands.registration",
    "commands.shop",
    "commands.wrapped",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildIdIssue {
    pub line: usize,
    pub filename: String,
    pub function: String,
    pub description: String,
}

impl fmt::Display for GuildIdIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.description)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceError {
    UnterminatedString,
    UnbalancedDelimiter { line: usize },
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedString => formatter.write_str("unterminated Python string literal"),
            Self::UnbalancedDelimiter { line } => {
                write!(formatter, "unbalanced Python delimiter at line {line}")
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandCatalogError {
    InvalidModuleName(String),
    MissingSource(String),
    EmptySource(String),
    InvalidSource { module: String, error: SourceError },
    MissingSetup(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Name(String),
    Open(char),
    Close(char),
    Comma,
    Equal,
    Dot,
    Star,
    Colon,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    line: usize,
    column: usize,
}

impl Token {
    fn is_name(&self, expected: &str) -> bool {
        matches!(&self.kind, TokenKind::Name(name) if name == expected)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FunctionSpan {
    name: String,
    start_line: usize,
    header_end_line: usize,
    end_line: usize,
}

/// Return all guild-ID definition-before-use issues in one Python module.
///
/// Like the Python AST guard, a function is reported only when it contains
/// both a direct call use and a later simple assignment. A function parameter
/// named `guild_id` is deliberately not treated as an assignment.
#[must_use]
pub fn find_guild_id_issues(filename: &str, source: &str) -> Vec<GuildIdIssue> {
    let masked = match mask_literals_and_comments(source) {
        Ok(masked) => masked,
        Err(error) => return vec![syntax_issue(filename, error)],
    };
    let tokens = tokenize(&masked);
    let delimiter_pairs = match delimiter_pairs(&tokens) {
        Ok(pairs) => pairs,
        Err(error) => return vec![syntax_issue(filename, error)],
    };
    let functions = function_spans(&masked);
    let calls = call_ranges(&tokens, &delimiter_pairs);

    functions
        .into_iter()
        .filter_map(|function| {
            let first_definition = tokens
                .iter()
                .enumerate()
                .filter(|(_, token)| {
                    token.line > function.header_end_line
                        && token.line <= function.end_line
                        && token.is_name("guild_id")
                })
                .filter(|(index, _)| {
                    matches!(
                        tokens.get(index + 1).map(|token| &token.kind),
                        Some(TokenKind::Equal)
                    ) && !inside_any_call(*index, &calls)
                })
                .map(|(_, token)| token.line)
                .min();
            let first_use = calls
                .iter()
                .filter(|range| {
                    let line = tokens[range.open].line;
                    line >= function.start_line && line <= function.end_line
                })
                .filter_map(|range| direct_guild_id_argument(&tokens, range))
                .min();

            match (first_use, first_definition) {
                (Some(use_line), Some(definition_line)) if use_line < definition_line => {
                    Some(GuildIdIssue {
                        line: use_line,
                        filename: filename.to_owned(),
                        function: function.name.clone(),
                        description: format!(
                            "{filename}:{}: guild_id used on line {use_line} before defined on line {definition_line}",
                            function.name
                        ),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn syntax_issue(filename: &str, error: SourceError) -> GuildIdIssue {
    GuildIdIssue {
        line: 0,
        filename: filename.to_owned(),
        function: String::new(),
        description: format!("Syntax error in {filename}: {error}"),
    }
}

/// Look up one compile-time command source by its Python filename.
#[must_use]
pub fn command_source_by_filename(filename: &str) -> Option<&'static CommandSource> {
    ALL_COMMAND_SOURCES
        .iter()
        .find(|command| command.filename == filename)
}

/// Validate the maintained import catalog without importing Python.
///
/// Compile-time `include_str!` proves every file exists. This function also
/// checks lexical completeness and the conventional async `setup` entrypoint
/// required by the Discord extension loader.
pub fn validate_maintained_command_catalog() -> Result<(), CommandCatalogError> {
    for module in MAINTAINED_COMMAND_MODULES {
        let Some(stem) = module.strip_prefix("commands.") else {
            return Err(CommandCatalogError::InvalidModuleName((*module).to_owned()));
        };
        if stem.is_empty() || stem.contains('.') {
            return Err(CommandCatalogError::InvalidModuleName((*module).to_owned()));
        }
        let filename = format!("{stem}.py");
        let Some(command) = command_source_by_filename(&filename) else {
            return Err(CommandCatalogError::MissingSource((*module).to_owned()));
        };
        if command.source.trim().is_empty() {
            return Err(CommandCatalogError::EmptySource((*module).to_owned()));
        }
        let masked = mask_literals_and_comments(command.source).map_err(|error| {
            CommandCatalogError::InvalidSource {
                module: (*module).to_owned(),
                error,
            }
        })?;
        let tokens = tokenize(&masked);
        delimiter_pairs(&tokens).map_err(|error| CommandCatalogError::InvalidSource {
            module: (*module).to_owned(),
            error,
        })?;
        if !function_spans(&masked)
            .iter()
            .any(|function| function.name == "setup")
        {
            return Err(CommandCatalogError::MissingSetup((*module).to_owned()));
        }
    }
    Ok(())
}

fn mask_literals_and_comments(source: &str) -> Result<String, SourceError> {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
        Code,
        Comment,
        Quoted { delimiter: u8, triple: bool },
    }

    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let mut state = State::Code;
    let mut index = 0;
    while index < bytes.len() {
        match state {
            State::Code => match bytes[index] {
                b'#' => {
                    masked[index] = b' ';
                    state = State::Comment;
                    index += 1;
                }
                delimiter @ (b'\'' | b'"') => {
                    let triple = bytes.get(index + 1) == Some(&delimiter)
                        && bytes.get(index + 2) == Some(&delimiter);
                    let width = if triple { 3 } else { 1 };
                    masked[index..index + width].fill(b' ');
                    state = State::Quoted { delimiter, triple };
                    index += width;
                }
                _ => index += 1,
            },
            State::Comment => {
                if bytes[index] == b'\n' {
                    state = State::Code;
                } else {
                    masked[index] = b' ';
                }
                index += 1;
            }
            State::Quoted { delimiter, triple } => {
                if triple
                    && bytes[index] == delimiter
                    && bytes.get(index + 1) == Some(&delimiter)
                    && bytes.get(index + 2) == Some(&delimiter)
                {
                    masked[index..index + 3].fill(b' ');
                    state = State::Code;
                    index += 3;
                } else if !triple && bytes[index] == delimiter {
                    masked[index] = b' ';
                    state = State::Code;
                    index += 1;
                } else if bytes[index] == b'\\' {
                    masked[index] = b' ';
                    index += 1;
                    if let Some(byte) = bytes.get(index) {
                        if *byte != b'\n' {
                            masked[index] = b' ';
                        }
                        index += 1;
                    }
                } else {
                    if bytes[index] != b'\n' {
                        masked[index] = b' ';
                    }
                    index += 1;
                }
            }
        }
    }
    if matches!(state, State::Quoted { .. }) {
        return Err(SourceError::UnterminatedString);
    }
    String::from_utf8(masked).map_err(|_| SourceError::UnterminatedString)
}

fn tokenize(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;
    let mut column = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            line += 1;
            column = 0;
            index += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            index += 1;
            column += 1;
            continue;
        }
        let token_line = line;
        let token_column = column;
        let kind = if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while bytes
                .get(index)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                index += 1;
                column += 1;
            }
            TokenKind::Name(source[start..index].to_owned())
        } else {
            index += 1;
            column += 1;
            match byte {
                b'(' | b'[' | b'{' => TokenKind::Open(char::from(byte)),
                b')' | b']' | b'}' => TokenKind::Close(char::from(byte)),
                b',' => TokenKind::Comma,
                b'.' => TokenKind::Dot,
                b'*' => TokenKind::Star,
                b':' => TokenKind::Colon,
                b'=' if bytes.get(index) != Some(&b'=') => TokenKind::Equal,
                _ => TokenKind::Other,
            }
        };
        tokens.push(Token {
            kind,
            line: token_line,
            column: token_column,
        });
    }
    tokens
}

fn delimiter_pairs(tokens: &[Token]) -> Result<Vec<(usize, usize)>, SourceError> {
    let mut stack = Vec::<(char, usize)>::new();
    let mut pairs = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::Open(delimiter) => stack.push((delimiter, index)),
            TokenKind::Close(delimiter) => {
                let Some((open, open_index)) = stack.pop() else {
                    return Err(SourceError::UnbalancedDelimiter { line: token.line });
                };
                if matching_close(open) != delimiter {
                    return Err(SourceError::UnbalancedDelimiter { line: token.line });
                }
                pairs.push((open_index, index));
            }
            _ => {}
        }
    }
    if let Some((_, index)) = stack.pop() {
        return Err(SourceError::UnbalancedDelimiter {
            line: tokens[index].line,
        });
    }
    Ok(pairs)
}

const fn matching_close(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => open,
    }
}

fn function_spans(source: &str) -> Vec<FunctionSpan> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut functions = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(name) = function_name(trimmed) else {
            continue;
        };
        let indentation = line.len() - trimmed.len();
        let header_end = function_header_end(&lines, index);
        let end_line = lines
            .iter()
            .enumerate()
            .skip(header_end + 1)
            .find_map(|(next_index, next_line)| {
                let next_trimmed = next_line.trim_start();
                if next_trimmed.is_empty() {
                    return None;
                }
                let next_indentation = next_line.len() - next_trimmed.len();
                (next_indentation <= indentation).then_some(next_index)
            })
            .unwrap_or(lines.len());
        functions.push(FunctionSpan {
            name,
            start_line: index + 1,
            header_end_line: header_end + 1,
            end_line,
        });
    }
    functions
}

fn function_name(line: &str) -> Option<String> {
    let line = line.strip_prefix("async ").unwrap_or(line);
    let remainder = line.strip_prefix("def ")?;
    let name = remainder
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    (name > 0).then(|| remainder[..name].to_owned())
}

fn function_header_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0_i64;
    for (index, line) in lines.iter().enumerate().skip(start) {
        for byte in line.bytes() {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b':' if depth == 0 => return index,
                _ => {}
            }
        }
    }
    start
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CallRange {
    open: usize,
    close: usize,
}

fn call_ranges(tokens: &[Token], pairs: &[(usize, usize)]) -> Vec<CallRange> {
    pairs
        .iter()
        .filter_map(|(open, close)| {
            matches!(tokens[*open].kind, TokenKind::Open('('))
                .then(|| is_call_open(tokens, *open))
                .and_then(|is_call| {
                    is_call.then_some(CallRange {
                        open: *open,
                        close: *close,
                    })
                })
        })
        .collect()
}

fn is_call_open(tokens: &[Token], open: usize) -> bool {
    let Some(previous) = open.checked_sub(1).and_then(|index| tokens.get(index)) else {
        return false;
    };
    match &previous.kind {
        TokenKind::Name(name) => {
            const NON_CALL_NAMES: &[&str] = &[
                "and", "assert", "class", "def", "del", "elif", "except", "for", "if", "in", "is",
                "lambda", "match", "not", "or", "raise", "return", "while", "with", "yield",
            ];
            if NON_CALL_NAMES.contains(&name.as_str()) {
                return false;
            }
            !open
                .checked_sub(2)
                .and_then(|index| tokens.get(index))
                .is_some_and(|token| token.is_name("def") || token.is_name("class"))
        }
        TokenKind::Close(')' | ']') => true,
        _ => false,
    }
}

fn inside_any_call(index: usize, calls: &[CallRange]) -> bool {
    calls
        .iter()
        .any(|range| range.open < index && index < range.close)
}

fn direct_guild_id_argument(tokens: &[Token], call: &CallRange) -> Option<usize> {
    let mut segment_start = call.open + 1;
    let mut nested = 0_i64;
    for index in call.open + 1..=call.close {
        let at_end = index == call.close;
        let separator = !at_end && nested == 0 && matches!(tokens[index].kind, TokenKind::Comma);
        if at_end || separator {
            if direct_guild_id_segment(&tokens[segment_start..index]) {
                return Some(tokens[call.open].line);
            }
            segment_start = index + 1;
            continue;
        }
        match tokens[index].kind {
            TokenKind::Open(_) => nested += 1,
            TokenKind::Close(_) => nested -= 1,
            _ => {}
        }
    }
    None
}

fn direct_guild_id_segment(segment: &[Token]) -> bool {
    (segment.len() == 1 && segment[0].is_name("guild_id"))
        || (segment.len() == 3
            && segment[0].is_name("guild_id")
            && matches!(segment[1].kind, TokenKind::Equal)
            && segment[2].is_name("guild_id"))
}

#[cfg(test)]
mod tests;
