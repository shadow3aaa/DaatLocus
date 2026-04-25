//! Interactive configuration wizard for first-run setup and `config` subcommands.

use std::{collections::HashMap, time::Duration};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use miette::{Result, miette};
use ratatui::{
    DefaultTerminal, Frame, TerminalOptions, Viewport,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    config::{
        Config, JudgeConfig, ModelConfig, ProviderConfig, normalize_provider_base_url,
        redact_secret_text, resolve_env_reference, write_config,
    },
    i18n::Locale,
    model_catalog::{ModelCapacity, catalog_model_capacity, conservative_model_capacity},
};

// ---------------------------------------------------------------------------
// GitHub OAuth device code flow
// ---------------------------------------------------------------------------

// Public Client ID used by the official GitHub Copilot app.
// Tokens from this flow can be exchanged through copilot_internal/v2/token
// for the session token that exposes the full Copilot model set.
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Run the GitHub OAuth device code flow and return an access token.
async fn run_github_device_flow(locale: Locale) -> Result<String> {
    let client_id = GITHUB_CLIENT_ID;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.http_client_failed", error = e)
            )
        })?;

    println!("  {}", crate::tr!(locale, "github.request_device_code"));
    let resp = http
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={}&scope=read%3Auser", urlenc(client_id)))
        .send()
        .await
        .map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.request_device_code_failed", error = e)
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(miette!(
            "{}",
            crate::tr!(locale, "github.http_error", status = status, body = body)
        ));
    }

    let device: serde_json::Value = resp.json().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(locale, "github.parse_device_code_failed", error = e)
        )
    })?;

    let device_code = device["device_code"]
        .as_str()
        .ok_or_else(|| miette!("{}", crate::tr!(locale, "github.missing_device_code")))?
        .to_string();
    let user_code = device["user_code"]
        .as_str()
        .ok_or_else(|| miette!("{}", crate::tr!(locale, "github.missing_user_code")))?
        .to_string();
    let verification_uri = device["verification_uri"]
        .as_str()
        .unwrap_or("https://github.com/login/device")
        .to_string();
    let expires_in = device["expires_in"].as_u64().unwrap_or(900);
    let interval_secs = device["interval"].as_u64().unwrap_or(5).max(5);

    println!();
    println!("  {}", crate::tr!(locale, "github.authorization"));
    println!(
        "  1. {}",
        crate::tr!(locale, "github.open_url", url = verification_uri)
    );
    println!(
        "  2. {}",
        crate::tr!(locale, "github.enter_code", code = user_code)
    );
    println!();

    let _ = open_browser(&verification_uri);

    let expires_at = std::time::Instant::now() + Duration::from_secs(expires_in);
    let poll_interval = Duration::from_secs(interval_secs);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("{}", crate::tr!(locale, "github.expired")));
        }

        tokio::time::sleep(poll_interval).await;

        dots = (dots + 1) % 4;
        print!(
            "\r  {}",
            crate::tr!(locale, "github.waiting", dots = ".".repeat(dots + 1))
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let poll_resp = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id={}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                urlenc(client_id),
                urlenc(&device_code),
            ))
            .send()
            .await
            .map_err(|e| miette!("{}", crate::tr!(locale, "github.poll_failed", error = e)))?;

        let body: serde_json::Value = poll_resp.json().await.map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.parse_token_failed", error = e)
            )
        })?;

        if let Some(token) = body["access_token"].as_str() {
            println!(
                "\r  {}                                  ",
                crate::tr!(locale, "github.success")
            );
            return Ok(token.to_string());
        }

        match body["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                // GitHub asks clients to slow down by adding an extra delay.
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => {
                return Err(miette!("{}", crate::tr!(locale, "github.expired")));
            }
            Some("access_denied") => {
                return Err(miette!("{}", crate::tr!(locale, "github.access_denied")));
            }
            Some(other) => {
                return Err(miette!(
                    "{}",
                    crate::tr!(locale, "github.auth_error", error = other)
                ));
            }
            None => {
                return Err(miette!(
                    "{}",
                    crate::tr!(locale, "github.unknown_response", body = body)
                ));
            }
        }
    }
}

/// Minimal percent-encoding for client_id and device_code form fields.
fn urlenc(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c]
            }
            _ => format!("%{:02X}", c as u32).chars().collect(),
        })
        .collect()
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn()?
        .wait()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn info(msg: &str) {
    println!("  {msg}");
}

fn header(msg: &str) {
    println!("\n{msg}");
    println!("{}", "─".repeat(msg.len()));
}

fn prompt_cancelled(locale: Locale) -> miette::Report {
    miette!("{}", crate::tr!(locale, "common.cancelled"))
}

const PROMPT_VIEWPORT_HEIGHT: u16 = 14;

struct PromptUi {
    terminal: Option<DefaultTerminal>,
    locale: Locale,
}

impl PromptUi {
    fn new(locale: Locale) -> Result<Self> {
        let mut ui = Self {
            terminal: None,
            locale,
        };
        ui.resume()?;
        Ok(ui)
    }

    fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    fn locale(&self) -> Locale {
        self.locale
    }

    fn resume(&mut self) -> Result<()> {
        if self.terminal.is_none() {
            self.terminal = Some(
                ratatui::try_init_with_options(TerminalOptions {
                    viewport: Viewport::Inline(PROMPT_VIEWPORT_HEIGHT),
                })
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(self.locale, "prompt_ui.init_failed", error = e)
                    )
                })?,
            );
        }
        Ok(())
    }

    fn suspend(&mut self) {
        if self.terminal.take().is_some() {
            let _ = ratatui::try_restore();
        }
    }

    fn terminal_mut(&mut self) -> Result<&mut DefaultTerminal> {
        self.resume()?;
        Ok(self.terminal.as_mut().expect("prompt terminal initialized"))
    }

    fn select<T: AsRef<str>>(
        &mut self,
        prompt: &str,
        items: &[T],
        default: usize,
    ) -> Result<usize> {
        if items.is_empty() {
            return Err(miette!(
                "{}",
                crate::tr!(self.locale, "prompt_ui.internal_empty_options")
            ));
        }

        let mut state = ListState::default().with_selected(Some(default.min(items.len() - 1)));

        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| render_select_prompt(frame, locale, prompt, items, &mut state))
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current == 0 {
                        items.len() - 1
                    } else {
                        current - 1
                    };
                    state.select(Some(next));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current + 1 >= items.len() {
                        0
                    } else {
                        current + 1
                    };
                    state.select(Some(next));
                }
                KeyCode::Enter => return Ok(state.selected().unwrap_or(0)),
                KeyCode::Esc => return Err(prompt_cancelled(self.locale)),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                _ => {}
            }
        }
    }

    fn text(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        self.text_inner(prompt, default.unwrap_or_default().to_string(), false, None)
    }

    fn password(&mut self, prompt: &str) -> Result<String> {
        self.text_inner(prompt, String::new(), true, None)
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        let items = [
            crate::tr!(self.locale, "common.yes"),
            crate::tr!(self.locale, "common.no"),
        ];
        Ok(self.select(prompt, &items, if default { 0 } else { 1 })? == 0)
    }

    fn usize(&mut self, prompt: &str, default: usize) -> Result<usize> {
        let mut current = default.to_string();
        let mut error: Option<String> = None;
        loop {
            let raw = self.text_inner(prompt, current, false, error.as_deref())?;
            match raw.trim().parse::<usize>() {
                Ok(value) => return Ok(value),
                Err(_) => {
                    current = raw;
                    error = Some(crate::tr!(self.locale, "prompt_ui.non_negative_integer"));
                }
            }
        }
    }

    fn text_inner(
        &mut self,
        prompt: &str,
        initial: String,
        secret: bool,
        error: Option<&str>,
    ) -> Result<String> {
        let mut value = initial;
        let mut cursor = value.len();

        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| {
                    render_text_prompt(frame, locale, prompt, &value, cursor, secret, error)
                })
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                KeyCode::Esc => return Err(prompt_cancelled(self.locale)),
                KeyCode::Enter => return Ok(value),
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.insert(cursor, ch);
                    cursor += ch.len_utf8();
                }
                KeyCode::Backspace if cursor > 0 => {
                    let prev = previous_char_boundary(&value, cursor);
                    value.drain(prev..cursor);
                    cursor = prev;
                }
                KeyCode::Delete if cursor < value.len() => {
                    let next = next_char_boundary(&value, cursor);
                    value.drain(cursor..next);
                }
                KeyCode::Left => {
                    cursor = previous_char_boundary(&value, cursor);
                }
                KeyCode::Right => {
                    cursor = next_char_boundary(&value, cursor);
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = value.len(),
                _ => {}
            }
        }
    }

    fn loading(&mut self, prompt: &str, note: &str) -> Result<()> {
        let locale = self.locale;
        self.terminal_mut()?
            .draw(|frame| render_loading_prompt(frame, locale, prompt, note))
            .map(|_| ())
            .map_err(|e| {
                miette!(
                    "{}",
                    crate::tr!(locale, "prompt_ui.render_failed", error = e)
                )
            })
    }

    fn detail(&mut self, prompt: &str, lines: &[String]) -> Result<()> {
        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| render_detail_prompt(frame, locale, prompt, lines))
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                _ => {}
            }
        }
    }
}

impl Drop for PromptUi {
    fn drop(&mut self) {
        self.suspend();
    }
}

fn read_prompt_key() -> Result<crossterm::event::KeyEvent> {
    loop {
        let event = event::read().map_err(|e| miette!("failed to read terminal input: {e}"))?;
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            return Ok(key);
        }
    }
}

fn prompt_panel_block(locale: Locale) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![
            Span::styled(
                "Config",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", crate::tr!(locale, "prompt_ui.inline")),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
}

fn render_select_prompt<T: AsRef<str>>(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    items: &[T],
    state: &mut ListState,
) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, list_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.select"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.option_count", count = items.len()),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|item| {
            ListItem::new(Line::from(vec![
                Span::styled("· ", Style::default().fg(Color::DarkGray)),
                Span::styled(item.as_ref().to_string(), Style::default().fg(Color::Gray)),
            ]))
        })
        .collect();
    let list = List::new(list_items)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, list_area, state);
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_select"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_text_prompt(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    value: &str,
    cursor: usize,
    secret: bool,
    error: Option<&str>,
) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, input_area, help_area, note_area] = inner.layout(&layout);

    let display = if secret {
        "*".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let input = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(display),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if secret {
                    crate::tr!(locale, "prompt_ui.secret")
                } else {
                    crate::tr!(locale, "prompt_ui.input")
                },
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.enter_confirm"),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );

    let field_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Cyan)
        })
        .title(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.value"),
            Style::default().fg(Color::DarkGray),
        )));
    let field_inner = field_block.inner(input_area);
    frame.render_widget(
        Paragraph::new(input)
            .block(field_block)
            .wrap(Wrap { trim: false }),
        input_area,
    );
    frame.set_cursor_position((
        field_inner.x + 2 + value[..cursor].chars().count() as u16,
        field_inner.y,
    ));

    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_text"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
    frame.render_widget(
        Paragraph::new(match error {
            Some(error) => Line::from(Span::styled(
                error.to_string(),
                Style::default().fg(Color::Red),
            )),
            None if secret => Line::from(Span::styled(
                crate::tr!(locale, "prompt_ui.masked"),
                Style::default().fg(Color::DarkGray),
            )),
            None => Line::from(Span::styled(
                crate::tr!(locale, "prompt_ui.plain"),
                Style::default().fg(Color::DarkGray),
            )),
        }),
        note_area,
    );
}

fn render_loading_prompt(frame: &mut Frame, locale: Locale, prompt: &str, note: &str) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, body_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.loading"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            note.to_string(),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(
                crate::tr!(locale, "prompt_ui.loading_body"),
                Style::default().fg(Color::White),
            ),
        ])),
        body_area,
    );
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.loading_help"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_detail_prompt(frame: &mut Frame, locale: Locale, prompt: &str, lines: &[String]) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, body_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.detail"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.line_count", count = lines.len()),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );
    frame.render_widget(
        Paragraph::new(
            lines
                .iter()
                .map(|line| {
                    Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray)))
                })
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false }),
        body_area,
    );
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_detail"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn previous_char_boundary(s: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    s[..index]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    s[index..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| index + offset)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Provider wizard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    OpenAI,
    GithubCopilot,
    OpenAICompatible,
}

impl ProviderKind {
    fn labels(locale: Locale) -> Vec<String> {
        vec![
            "OpenAI".to_string(),
            "GitHub Copilot".to_string(),
            crate::tr!(locale, "config.provider_openai_compatible"),
        ]
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::OpenAI,
            1 => Self::GithubCopilot,
            _ => Self::OpenAICompatible,
        }
    }
}

/// Prompt for one provider definition and return its name and config.
async fn prompt_provider(
    ui: &mut PromptUi,
    existing_names: &[String],
) -> Result<(String, ProviderConfig)> {
    let locale = ui.locale();
    let labels = ProviderKind::labels(locale);
    let kind_idx = ui.select(&crate::tr!(locale, "config.provider_kind"), &labels, 0)?;
    let kind = ProviderKind::from_index(kind_idx);

    let default_name = match kind {
        ProviderKind::OpenAI => "openai",
        ProviderKind::GithubCopilot => "copilot",
        ProviderKind::OpenAICompatible => "local",
    };
    // Suffix duplicate defaults to avoid a collision.
    let default_name = if existing_names.contains(&default_name.to_string()) {
        format!("{}-2", default_name)
    } else {
        default_name.to_string()
    };

    let name = ui.text(
        &crate::tr!(locale, "config.provider_name"),
        Some(&default_name),
    )?;

    let provider = match kind {
        ProviderKind::OpenAI => {
            let api_key = ui.password(&crate::tr!(locale, "config.openai_api_key"))?;
            let use_custom_url =
                ui.confirm(&crate::tr!(locale, "config.custom_base_url"), false)?;
            let base_url = if use_custom_url {
                let url = ui.text(&crate::tr!(locale, "config.base_url_openai"), None)?;
                Some(normalize_provider_base_url(&url))
            } else {
                None
            };
            ProviderConfig::Openai { api_key, base_url }
        }
        ProviderKind::GithubCopilot => {
            let auth_method = ui.select(
                &crate::tr!(locale, "config.github_auth_method"),
                &[
                    crate::tr!(locale, "config.github_device_login"),
                    crate::tr!(locale, "config.github_manual_token"),
                    crate::tr!(locale, "config.github_env_token"),
                ],
                0,
            )?;

            let github_token = match auth_method {
                0 => {
                    ui.suspend();
                    let result = run_github_device_flow(locale).await;
                    ui.resume()?;
                    result?
                }
                1 => ui.password(&crate::tr!(locale, "config.github_token"))?,
                _ => "${GITHUB_TOKEN}".to_string(),
            };
            ProviderConfig::GithubCopilot { github_token }
        }
        ProviderKind::OpenAICompatible => {
            let base_url = ui.text(
                &crate::tr!(locale, "config.base_url_local"),
                Some("http://localhost:11434/v1"),
            )?;
            let api_key = ui.text(&crate::tr!(locale, "config.local_api_key"), Some("ollama"))?;
            ProviderConfig::OpenaiCompatible {
                base_url: normalize_provider_base_url(&base_url),
                api_key,
            }
        }
    };

    Ok((name, provider))
}

// ---------------------------------------------------------------------------
// Model discovery
// ---------------------------------------------------------------------------

/// Static fallback list of known GitHub Copilot models.
const COPILOT_DEFAULT_MODELS: &[&str] = &[
    "claude-sonnet-4.6",
    "claude-sonnet-4.5",
    "claude-opus-4.5",
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "o3-mini",
    "o1",
    "o1-mini",
];

fn resolve_model_capacity(
    model_id: &str,
    detected_context_window: Option<usize>,
    detected_max_output: Option<usize>,
) -> ModelCapacity {
    let catalog = catalog_model_capacity(model_id);
    let fallback = conservative_model_capacity();
    ModelCapacity {
        context_window_tokens: detected_context_window
            .or_else(|| catalog.map(|capacity| capacity.context_window_tokens))
            .unwrap_or(fallback.context_window_tokens),
        max_completion_tokens: detected_max_output
            .or_else(|| catalog.map(|capacity| capacity.max_completion_tokens))
            .unwrap_or(fallback.max_completion_tokens),
    }
}

/// Discover Copilot models via the internal session-token API, falling back to a static list.
async fn fetch_copilot_models(github_token: &str) -> Vec<DiscoveredModel> {
    let fallback = || {
        COPILOT_DEFAULT_MODELS
            .iter()
            .map(|s| DiscoveredModel {
                id: s.to_string(),
                context_window: None,
                max_output_tokens: None,
            })
            .collect::<Vec<_>>()
    };

    let token = resolve_env_reference(github_token);
    if token.is_empty() {
        tracing::warn!("copilot model discovery: github token empty, using static list");
        return fallback();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("copilot model discovery: http client error: {e}");
            return fallback();
        }
    };

    match try_fetch_via_session_token(&client, &token).await {
        Some(models) => {
            tracing::info!(
                "copilot model discovery: {} models via internal API",
                models.len()
            );
            models
        }
        None => {
            tracing::warn!(
                "copilot model discovery: session token exchange failed, using static list"
            );
            fallback()
        }
    }
}

async fn try_fetch_via_session_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Option<Vec<DiscoveredModel>> {
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        tracing::debug!(http_status = %resp.status(), "copilot model discovery: session token exchange failed");
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let session_token = json["token"].as_str()?.to_string();

    let base_url = session_token
        .split(';')
        .find_map(|part| {
            let trimmed = part.trim();
            let host = trimmed.strip_prefix("proxy-ep=").or_else(|| {
                if trimmed.to_lowercase().starts_with("proxy-ep=") {
                    Some(&trimmed[9..])
                } else {
                    None
                }
            })?;
            if host.is_empty() {
                return None;
            }
            let host = if host.to_lowercase().starts_with("proxy.") {
                format!("api.{}", &host[6..])
            } else {
                host.to_string()
            };
            Some(format!("https://{host}"))
        })
        .unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string());

    let models =
        fetch_copilot_internal_models(client, &format!("{base_url}/models"), &session_token).await;
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

/// Model metadata returned by the provider API.
#[derive(Debug, Clone)]
struct DiscoveredModel {
    id: String,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
}

/// Fetch provider model IDs. Failures return an empty list.
async fn fetch_model_ids(provider: &ProviderConfig) -> Vec<DiscoveredModel> {
    match provider {
        ProviderConfig::GithubCopilot { github_token } => fetch_copilot_models(github_token).await,
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base, &api_key).await
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base_url, &api_key).await
        }
    }
}

async fn fetch_openai_models(base_url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = format!("{}/models", normalize_provider_base_url(base_url));
    fetch_openai_models_path(&url, api_key).await
}

async fn fetch_openai_models_path(url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = url.to_string();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("fetch_openai_models: failed to build http client: {e}");
            return vec![];
        }
    };
    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %url, "fetch_openai_models: request failed: {e}");
            return vec![];
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, api_key);
        tracing::warn!(url = %url, http_status = %status, body = %body, "fetch_openai_models: non-2xx response");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

async fn fetch_copilot_internal_models(
    client: &reqwest::Client,
    url: &str,
    session_token: &str,
) -> Vec<DiscoveredModel> {
    let resp = match client
        .get(url)
        .header("Authorization", format!("Bearer {session_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url, "copilot internal models request failed: {e}");
            return vec![];
        }
    };
    if !resp.status().is_success() {
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        let b = redact_secret_text(&b, session_token);
        tracing::warn!(url, http_status = %s, body = %b, "copilot internal models non-2xx");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

fn parse_models_response(json: Option<serde_json::Value>) -> Vec<DiscoveredModel> {
    let json = match json {
        Some(j) => j,
        None => return vec![],
    };
    let mut models: Vec<DiscoveredModel> = json["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let limits = &m["capabilities"]["limits"];
            let context_window = limits["max_context_window_tokens"]
                .as_u64()
                .map(|v| v as usize);
            let max_output_tokens = limits["max_output_tokens"].as_u64().map(|v| v as usize);
            Some(DiscoveredModel {
                id,
                context_window,
                max_output_tokens,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

// ---------------------------------------------------------------------------
// Model wizard
// ---------------------------------------------------------------------------

/// Prompt for one model definition and return its name and config.
async fn prompt_model(
    ui: &mut PromptUi,
    provider_name: &str,
    provider: &ProviderConfig,
) -> Result<(String, ModelConfig)> {
    let locale = ui.locale();
    ui.loading(
        &crate::tr!(locale, "config.discover_models"),
        &format!("provider: {provider_name}"),
    )?;
    let discovered = fetch_model_ids(provider).await;

    let (model_id, api_ctx, api_out) = if discovered.is_empty() {
        let id = ui.text("Model ID", None)?;
        (id, None, None)
    } else {
        let manual = crate::tr!(locale, "config.manual_model");
        let labels: Vec<String> = discovered
            .iter()
            .map(|m| m.id.clone())
            .chain(std::iter::once(manual.clone()))
            .collect();

        let idx = ui.select(&crate::tr!(locale, "config.select_model"), &labels, 0)?;

        if labels[idx] == manual {
            let id = ui.text("Model ID", None)?;
            (id, None, None)
        } else {
            let m = &discovered[idx];
            (m.id.clone(), m.context_window, m.max_output_tokens)
        }
    };

    let capacity = resolve_model_capacity(&model_id, api_ctx, api_out);

    let default_name = model_id
        .split(['/', ':'])
        .next_back()
        .unwrap_or(&model_id)
        .to_string();
    let name = ui.text(
        &crate::tr!(locale, "config.model_name"),
        Some(&default_name),
    )?;

    let context_window = ui.usize("Context window tokens", capacity.context_window_tokens)?;

    let max_completion = ui.usize("Max completion tokens", capacity.max_completion_tokens)?;

    Ok((
        name,
        ModelConfig {
            provider: provider_name.to_string(),
            model_id,
            context_window_tokens: context_window,
            max_completion_tokens: max_completion,
            ..ModelConfig::default()
        },
    ))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the first-time setup wizard, write config.toml, and return the generated Config.
pub async fn run_first_time_setup() -> Result<Config> {
    println!();
    println!("Daat Locus setup");
    println!();

    let mut ui = PromptUi::new(Locale::default())?;
    let language_items = [
        crate::tr!(Locale::EnUs, "setup.language_english"),
        crate::tr!(Locale::ZhCn, "setup.language_chinese"),
    ];
    let language_idx = ui.select(
        &crate::tr!(Locale::EnUs, "setup.language_prompt"),
        &language_items,
        0,
    )?;
    let locale = Locale::from_language_setup_index(language_idx);
    ui.set_locale(locale);

    println!();
    println!("{}", crate::tr!(locale, "setup.welcome"));
    println!("{}", crate::tr!(locale, "setup.missing_config"));
    println!();

    let skip = ui.select(
        &crate::tr!(locale, "setup.init_mode"),
        &[
            crate::tr!(locale, "setup.interactive"),
            crate::tr!(locale, "setup.skip_default"),
        ],
        0,
    )?;

    if skip == 1 {
        let config = Config {
            locale,
            ..Config::default()
        };
        write_config(&config).await?;
        info(&crate::tr!(locale, "setup.default_created"));
        return Ok(config);
    }

    header(&crate::tr!(locale, "setup.provider_step"));
    let (provider_name, provider_config) = prompt_provider(&mut ui, &[]).await?;

    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider_config.clone());

    header(&crate::tr!(locale, "setup.model_step"));
    let (model_name, model_config) =
        prompt_model(&mut ui, &provider_name, &provider_config).await?;

    let mut models = HashMap::new();
    models.insert(model_name.clone(), model_config);

    let config = Config {
        locale,
        providers,
        models,
        main_model: model_name.clone(),
        judge: JudgeConfig::default(),
        ..Config::default()
    };

    write_config(&config).await?;

    println!();
    println!("{}", crate::tr!(locale, "setup.written"));
    println!("  main_model = \"{model_name}\" （provider: {provider_name}）");
    println!();

    Ok(config)
}

/// `config add-provider` subcommand.
pub async fn run_add_provider() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;

    header(&crate::tr!(locale, "config.add_provider"));
    let existing: Vec<String> = config.providers.keys().cloned().collect();
    let mut ui = PromptUi::new(locale)?;
    let (name, provider) = prompt_provider(&mut ui, &existing).await?;

    if config.providers.contains_key(&name) {
        let overwrite = ui.confirm(
            &crate::tr!(locale, "common.overwrite_provider", name = name.clone()),
            false,
        )?;
        if !overwrite {
            info(&crate::tr!(locale, "common.cancelled_action"));
            return Ok(());
        }
    }

    config.providers.insert(name.clone(), provider);
    write_config(&config).await?;
    info(&crate::tr!(locale, "config.provider_saved", name = name));
    Ok(())
}

/// `config add-model` subcommand.
pub async fn run_add_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;

    header(&crate::tr!(locale, "config.add_model"));
    let mut ui = PromptUi::new(locale)?;
    let provider_names: Vec<String> = config.providers.keys().cloned().collect();
    if provider_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_providers")));
    }
    let provider_idx = if provider_names.len() == 1 {
        0
    } else {
        ui.select(
            &crate::tr!(locale, "config.bind_provider"),
            &provider_names,
            0,
        )?
    };
    let provider_name = &provider_names[provider_idx];
    let provider_config = config.providers.get(provider_name).unwrap();
    let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;

    if config.models.contains_key(&name) {
        let overwrite = ui.confirm(
            &crate::tr!(locale, "common.overwrite_model", name = name.clone()),
            false,
        )?;
        if !overwrite {
            info(&crate::tr!(locale, "common.cancelled_action"));
            return Ok(());
        }
    }

    config.models.insert(name.clone(), model);

    let set_main = ui.confirm(
        &crate::tr!(locale, "config.set_as_main", name = name.clone()),
        false,
    )?;
    if set_main {
        config.main_model = name.clone();
    }

    write_config(&config).await?;
    info(&crate::tr!(locale, "config.model_saved", name = name));
    Ok(())
}

/// `config set-main-model` subcommand.
pub async fn run_set_main_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;
    let mut ui = PromptUi::new(locale)?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
    }

    let current_idx = model_names
        .iter()
        .position(|n| n == &config.main_model)
        .unwrap_or(0);

    let idx = ui.select(
        &crate::tr!(locale, "config.select_main_model"),
        &model_names,
        current_idx,
    )?;

    config.main_model = model_names[idx].clone();
    write_config(&config).await?;
    info(&crate::tr!(
        locale,
        "config.main_model_set",
        name = config.main_model.clone()
    ));
    Ok(())
}

/// `config show` subcommand. Prints the current config summary with secrets masked.
pub async fn show_config() -> Result<()> {
    let config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;

    for line in render_config_summary_lines(&config, config.locale) {
        println!("{line}");
    }
    println!();
    Ok(())
}

fn render_config_summary_lines(config: &Config, locale: Locale) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push(crate::tr!(locale, "config.locale_heading"));
    lines.push("──────".to_string());
    lines.push(format!(
        "  {} ({})",
        config.locale.as_str(),
        config.locale.display_name()
    ));
    lines.push(String::new());

    lines.push(crate::tr!(locale, "config.providers_heading"));
    lines.push("─────────".to_string());
    for (name, provider) in &config.providers {
        let desc = match provider {
            ProviderConfig::Openai { api_key, base_url } => {
                let masked = mask_secret(api_key);
                let url = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
                format!("openai  url={url}  key={masked}")
            }
            ProviderConfig::GithubCopilot { github_token } => {
                let masked = mask_secret(github_token);
                format!("github-copilot  token={masked}")
            }
            ProviderConfig::OpenaiCompatible { base_url, api_key } => {
                let masked = mask_secret(api_key);
                format!("openai-compatible  url={base_url}  key={masked}")
            }
        };
        lines.push(format!("  [{name}]  {desc}"));
    }

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.models_heading"));
    lines.push("──────".to_string());
    for (name, model) in &config.models {
        let main_mark = if name == &config.main_model {
            " ← main"
        } else {
            ""
        };
        lines.push(format!(
            "  [{name}]{main_mark}  provider={}  model_id={}  ctx={}  max_out={}",
            model.provider,
            model.model_id,
            model.context_window_tokens,
            model.max_completion_tokens
        ));
    }

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.judge_heading"));
    lines.push("─────".to_string());
    let judge_model = config.judge.model.as_deref().unwrap_or(&config.main_model);
    lines.push(format!(
        "  enabled={}  model={}  candidates={}  cases={}",
        config.judge.enabled,
        judge_model,
        config.judge.max_pairwise_candidates,
        config.judge.max_pairwise_cases
    ));

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.hindsight_heading"));
    lines.push("─────────".to_string());
    let hindsight_model = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    let fallback_mark = if config.hindsight.model.is_none() {
        crate::tr!(locale, "config.fallback_to_main")
    } else {
        String::new()
    };
    lines.push(format!(
        "  model={}{}  port={}  profile={}",
        hindsight_model, fallback_mark, config.hindsight.port, config.hindsight.profile,
    ));

    lines
}

/// `config` without a subcommand: interactive menu.
pub async fn run_config_menu() -> Result<()> {
    let initial_locale = crate::config::load_config()
        .await
        .ok()
        .map(|config| config.locale)
        .unwrap_or_default();
    let mut ui = PromptUi::new(initial_locale)?;
    loop {
        let mut locale = ui.locale();
        let has_config = crate::config::config_file_exists().await;
        let status = if has_config {
            match crate::config::load_config().await {
                Ok(cfg) => {
                    locale = cfg.locale;
                    ui.set_locale(locale);
                    crate::tr!(
                        locale,
                        "config.status_configured",
                        main_model = cfg.main_model,
                        providers = cfg.providers.len(),
                        models = cfg.models.len(),
                        locale_name = cfg.locale.display_name()
                    )
                }
                Err(e) => crate::tr!(locale, "config.status_load_error", error = e),
            }
        } else {
            crate::tr!(locale, "config.status_unconfigured")
        };

        let items = [
            crate::tr!(locale, "config.show_details"),
            crate::tr!(locale, "config.add_provider"),
            crate::tr!(locale, "config.add_model"),
            crate::tr!(locale, "config.change_main_model"),
            crate::tr!(locale, "config.change_hindsight_model"),
            crate::tr!(locale, "config.exit"),
        ];

        let idx = ui.select(
            &crate::tr!(locale, "config.menu_title", status = status),
            &items,
            0,
        )?;

        match idx {
            0 => match crate::config::load_config().await {
                Ok(cfg) => ui.detail(
                    &crate::tr!(locale, "config.details_title"),
                    &render_config_summary_lines(&cfg, locale),
                )?,
                Err(e) => ui.detail(
                    &crate::tr!(locale, "config.details_title"),
                    &[crate::tr!(locale, "common.config_load_failed", error = e)],
                )?,
            },
            1 => {
                let mut config = crate::config::load_config().await.map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "common.config_load_failed", error = e)
                    )
                })?;
                let existing: Vec<String> = config.providers.keys().cloned().collect();
                let (name, provider) = prompt_provider(&mut ui, &existing).await?;
                if config.providers.contains_key(&name)
                    && !ui.confirm(
                        &crate::tr!(locale, "common.overwrite_provider", name = name.clone()),
                        false,
                    )?
                {
                    continue;
                }
                config.providers.insert(name, provider);
                write_config(&config).await?;
            }
            2 => {
                let mut config = crate::config::load_config().await.map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "common.config_load_failed", error = e)
                    )
                })?;
                let provider_names: Vec<String> = config.providers.keys().cloned().collect();
                if provider_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("{}", crate::tr!(locale, "common.no_providers")));
                }
                let provider_idx = if provider_names.len() == 1 {
                    0
                } else {
                    ui.select(
                        &crate::tr!(locale, "config.bind_provider"),
                        &provider_names,
                        0,
                    )?
                };
                let provider_name = &provider_names[provider_idx];
                let provider_config = config.providers.get(provider_name).unwrap();
                let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;
                if config.models.contains_key(&name)
                    && !ui.confirm(
                        &crate::tr!(locale, "common.overwrite_model", name = name.clone()),
                        false,
                    )?
                {
                    continue;
                }
                config.models.insert(name.clone(), model);
                if ui.confirm(
                    &crate::tr!(locale, "config.set_as_main", name = name.clone()),
                    false,
                )? {
                    config.main_model = name;
                }
                write_config(&config).await?;
            }
            3 => {
                let mut config = crate::config::load_config().await.map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "common.config_load_failed", error = e)
                    )
                })?;
                let model_names: Vec<String> = config.models.keys().cloned().collect();
                if model_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
                }
                let current_idx = model_names
                    .iter()
                    .position(|n| n == &config.main_model)
                    .unwrap_or(0);
                let idx = ui.select(
                    &crate::tr!(locale, "config.select_main_model"),
                    &model_names,
                    current_idx,
                )?;
                config.main_model = model_names[idx].clone();
                write_config(&config).await?;
            }
            4 => {
                let mut config = crate::config::load_config().await.map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "common.config_load_failed", error = e)
                    )
                })?;
                let model_names: Vec<String> = config.models.keys().cloned().collect();
                if model_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
                }
                let mut items: Vec<String> = model_names.clone();
                let use_main = crate::tr!(locale, "config.use_main_model");
                items.push(use_main.clone());
                let current_idx = config
                    .hindsight
                    .model
                    .as_ref()
                    .and_then(|m| model_names.iter().position(|n| n == m))
                    .unwrap_or(items.len() - 1);
                let idx = ui.select(
                    &crate::tr!(locale, "config.select_hindsight_model"),
                    &items,
                    current_idx,
                )?;
                config.hindsight.model = if items[idx] == use_main {
                    None
                } else {
                    Some(model_names[idx].clone())
                };
                write_config(&config).await?;
            }
            _ => break,
        }
    }
    Ok(())
}

/// `config set-hindsight-model` subcommand.
pub async fn run_set_hindsight_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;
    let mut ui = PromptUi::new(locale)?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
    }

    let mut items: Vec<String> = model_names.clone();
    let use_main = crate::tr!(locale, "config.use_main_model");
    items.push(use_main.clone());

    let current_idx = config
        .hindsight
        .model
        .as_ref()
        .and_then(|m| model_names.iter().position(|n| n == m))
        .unwrap_or(items.len() - 1);

    let idx = ui.select(
        &crate::tr!(locale, "config.select_hindsight_model"),
        &items,
        current_idx,
    )?;

    config.hindsight.model = if items[idx] == use_main {
        None
    } else {
        Some(model_names[idx].clone())
    };

    write_config(&config).await?;
    let display = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    info(&crate::tr!(
        locale,
        "config.hindsight_model_set",
        name = display
    ));
    Ok(())
}

fn mask_secret(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    // Show the first and last four characters.
    let prefix = &s[..4];
    let suffix = &s[s.len() - 4..];
    format!("{prefix}...{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_capacity_prefers_detected_values() {
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), Some(678));

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 678,
            }
        );
    }

    #[test]
    fn model_capacity_fills_missing_detected_fields_from_exact_catalog_match() {
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), None);

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 32_768,
            }
        );
    }

    #[test]
    fn model_capacity_uses_conservative_defaults_for_unknown_models() {
        let capacity = resolve_model_capacity("unknown-local-model", None, None);

        assert_eq!(capacity, conservative_model_capacity());
    }

    #[test]
    fn model_catalog_does_not_substring_match_similar_model_names() {
        let capacity = resolve_model_capacity("gpt-4.1-custom", None, None);

        assert_eq!(capacity, conservative_model_capacity());
    }
}
