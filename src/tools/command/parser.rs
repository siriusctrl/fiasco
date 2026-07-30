use anyhow::{Result, bail, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Invocation {
    pub stages: Vec<Vec<String>>,
    pub redirect: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Pipe,
    Redirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

pub(super) fn parse(command: &str) -> Result<Invocation> {
    let mut tokens = lex(command)?;
    ensure!(!tokens.is_empty(), "fiasco command must not be empty");

    let redirect = match tokens.iter().position(|token| *token == Token::Redirect) {
        Some(index) => {
            ensure!(
                index + 2 == tokens.len(),
                "`>` is supported only once at the end as `> <path>`"
            );
            let path = match tokens.pop().expect("redirect path") {
                Token::Word(path) => path,
                _ => unreachable!("redirect path shape was checked"),
            };
            tokens.pop();
            Some(path)
        }
        None => None,
    };
    ensure!(
        !tokens.contains(&Token::Redirect),
        "`>` is supported only once at the end"
    );

    let mut stages = Vec::new();
    let mut words = Vec::new();
    for token in tokens {
        match token {
            Token::Word(word) => words.push(word),
            Token::Pipe => {
                ensure!(!words.is_empty(), "pipeline contains an empty command");
                stages.push(std::mem::take(&mut words));
            }
            Token::Redirect => unreachable!("redirect tokens were removed"),
        }
    }
    ensure!(!words.is_empty(), "pipeline ends with an empty command");
    stages.push(words);

    Ok(Invocation { stages, redirect })
}

fn lex(command: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = Quote::None;
    let mut characters = command.chars();

    while let Some(character) = characters.next() {
        match quote {
            Quote::None => match character {
                '\'' => {
                    quote = Quote::Single;
                    started = true;
                }
                '"' => {
                    quote = Quote::Double;
                    started = true;
                }
                '\\' => {
                    let escaped = characters
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("command ends with an escape"))?;
                    word.push(escaped);
                    started = true;
                }
                '|' => {
                    finish_word(&mut tokens, &mut word, &mut started);
                    tokens.push(Token::Pipe);
                }
                '>' => {
                    finish_word(&mut tokens, &mut word, &mut started);
                    tokens.push(Token::Redirect);
                }
                character if character.is_whitespace() => {
                    finish_word(&mut tokens, &mut word, &mut started);
                }
                _ => {
                    word.push(character);
                    started = true;
                }
            },
            Quote::Single => {
                if character == '\'' {
                    quote = Quote::None;
                } else {
                    word.push(character);
                }
            }
            Quote::Double => match character {
                '"' => quote = Quote::None,
                '\\' => {
                    let escaped = characters
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("double quote ends with an escape"))?;
                    word.push(escaped);
                }
                _ => word.push(character),
            },
        }
    }

    match quote {
        Quote::None => {}
        Quote::Single => bail!("unterminated single quote"),
        Quote::Double => bail!("unterminated double quote"),
    }
    finish_word(&mut tokens, &mut word, &mut started);
    Ok(tokens)
}

fn finish_word(tokens: &mut Vec<Token>, word: &mut String, started: &mut bool) {
    if *started {
        tokens.push(Token::Word(std::mem::take(word)));
        *started = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_words_quotes_pipeline_and_terminal_redirect() {
        assert_eq!(
            parse(r#"history search "a | b" | web search query=- > "out file.json""#).unwrap(),
            Invocation {
                stages: vec![
                    vec!["history".into(), "search".into(), "a | b".into()],
                    vec!["web".into(), "search".into(), "query=-".into()],
                ],
                redirect: Some("out file.json".into()),
            }
        );
    }

    #[test]
    fn preserves_empty_quoted_words_and_escaped_operators() {
        assert_eq!(
            parse(r#"skill load "" \| \>"#).unwrap(),
            Invocation {
                stages: vec![vec![
                    "skill".into(),
                    "load".into(),
                    String::new(),
                    "|".into(),
                    ">".into(),
                ]],
                redirect: None,
            }
        );
    }

    #[test]
    fn rejects_general_shell_and_malformed_pipeline_shapes() {
        for command in [
            "",
            "| history search x",
            "history search x |",
            "history search x || web search",
            "history search x >> out",
            "history search x > out extra",
            "history search x > out > other",
            "'unterminated",
            r#"history search "unterminated"#,
            "history search trailing\\",
        ] {
            assert!(parse(command).is_err(), "{command:?} unexpectedly parsed");
        }
    }
}
