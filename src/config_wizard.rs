//! 交互式配置向导：首次运行 setup 和 `config` 子命令

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
        Config, JudgeConfig, ModelConfig, ProviderConfig, normalize_provider_base_url, write_config,
    },
    model_catalog::{ModelCapacity, catalog_model_capacity, conservative_model_capacity},
};

// ---------------------------------------------------------------------------
// GitHub OAuth device code flow
// ---------------------------------------------------------------------------

// GitHub Copilot 官方应用的公开 Client ID。
// 使用此 ID 获取的 token 可访问 copilot_internal/v2/token 换取 session token，
// 从而使用全量 Copilot 模型（包括 Claude）。
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// 执行 GitHub OAuth device code 流程，返回 access token。
async fn run_github_device_flow() -> Result<String> {
    let client_id = GITHUB_CLIENT_ID;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| miette!("HTTP client 初始化失败: {e}"))?;

    // Step 1: 请求 device code
    println!("  正在向 GitHub 请求 device code...");
    let resp = http
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&scope=read%3Auser",
            urlenc(&client_id)
        ))
        .send()
        .await
        .map_err(|e| miette!("请求 device code 失败: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(miette!("GitHub 返回错误 HTTP {status}: {body}"));
    }

    let device: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| miette!("解析 device code 响应失败: {e}"))?;

    let device_code = device["device_code"]
        .as_str()
        .ok_or_else(|| miette!("响应缺少 device_code 字段"))?
        .to_string();
    let user_code = device["user_code"]
        .as_str()
        .ok_or_else(|| miette!("响应缺少 user_code 字段"))?
        .to_string();
    let verification_uri = device["verification_uri"]
        .as_str()
        .unwrap_or("https://github.com/login/device")
        .to_string();
    let expires_in = device["expires_in"].as_u64().unwrap_or(900);
    let interval_secs = device["interval"].as_u64().unwrap_or(5).max(5);

    // Step 2: 提示用户在浏览器操作
    println!();
    println!("  GitHub 授权");
    println!("  1. 打开: {}", verification_uri);
    println!("  2. 输入验证码: {}", user_code);
    println!();

    // 尝试自动打开浏览器（失败则静默）
    let _ = open_browser(&verification_uri);

    // Step 3: 轮询直到用户完成授权
    let expires_at = std::time::Instant::now() + Duration::from_secs(expires_in);
    let poll_interval = Duration::from_secs(interval_secs);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("device code 已过期，请重新运行"));
        }

        tokio::time::sleep(poll_interval).await;

        dots = (dots + 1) % 4;
        print!("\r  等待 GitHub 授权{}   ", ".".repeat(dots + 1));
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let poll_resp = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id={}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                urlenc(&client_id),
                urlenc(&device_code),
            ))
            .send()
            .await
            .map_err(|e| miette!("轮询 access token 失败: {e}"))?;

        let body: serde_json::Value = poll_resp
            .json()
            .await
            .map_err(|e| miette!("解析 token 响应失败: {e}"))?;

        if let Some(token) = body["access_token"].as_str() {
            println!("\r  授权成功                                  ");
            return Ok(token.to_string());
        }

        match body["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                // GitHub 要求降速：额外等待 5 秒
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => return Err(miette!("device code 已过期，请重新运行")),
            Some("access_denied") => return Err(miette!("用户取消了授权")),
            Some(other) => return Err(miette!("GitHub 授权错误: {other}")),
            None => return Err(miette!("未知的 token 响应: {body}")),
        }
    }
}

/// 简单的 percent-encoding（仅编码非 unreserved 字符，满足 client_id / device_code 需求）
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
// 工具函数
// ---------------------------------------------------------------------------

/// 格式化输出一条信息行
fn info(msg: &str) {
    println!("  {msg}");
}

fn header(msg: &str) {
    println!("\n{msg}");
    println!("{}", "─".repeat(msg.len()));
}

fn prompt_cancelled() -> miette::Report {
    miette!("交互中断")
}

const PROMPT_VIEWPORT_HEIGHT: u16 = 14;

struct PromptUi {
    terminal: Option<DefaultTerminal>,
}

impl PromptUi {
    fn new() -> Result<Self> {
        let mut ui = Self { terminal: None };
        ui.resume()?;
        Ok(ui)
    }

    fn resume(&mut self) -> Result<()> {
        if self.terminal.is_none() {
            self.terminal = Some(
                ratatui::try_init_with_options(TerminalOptions {
                    viewport: Viewport::Inline(PROMPT_VIEWPORT_HEIGHT),
                })
                .map_err(|e| miette!("初始化终端 UI 失败: {e}"))?,
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
            return Err(miette!("内部错误：选项列表为空"));
        }

        let mut state = ListState::default().with_selected(Some(default.min(items.len() - 1)));

        loop {
            self.terminal_mut()?
                .draw(|frame| render_select_prompt(frame, prompt, items, &mut state))
                .map_err(|e| miette!("渲染终端 UI 失败: {e}"))?;

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
                KeyCode::Esc => return Err(prompt_cancelled()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled());
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
        Ok(self.select(prompt, &["是", "否"], if default { 0 } else { 1 })? == 0)
    }

    fn usize(&mut self, prompt: &str, default: usize) -> Result<usize> {
        let mut current = default.to_string();
        let mut error = None;
        loop {
            let raw = self.text_inner(prompt, current, false, error)?;
            match raw.trim().parse::<usize>() {
                Ok(value) => return Ok(value),
                Err(_) => {
                    current = raw;
                    error = Some("请输入非负整数");
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
            self.terminal_mut()?
                .draw(|frame| render_text_prompt(frame, prompt, &value, cursor, secret, error))
                .map_err(|e| miette!("渲染终端 UI 失败: {e}"))?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled());
                }
                KeyCode::Esc => return Err(prompt_cancelled()),
                KeyCode::Enter => return Ok(value),
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.insert(cursor, ch);
                    cursor += ch.len_utf8();
                }
                KeyCode::Backspace => {
                    if cursor > 0 {
                        let prev = previous_char_boundary(&value, cursor);
                        value.drain(prev..cursor);
                        cursor = prev;
                    }
                }
                KeyCode::Delete => {
                    if cursor < value.len() {
                        let next = next_char_boundary(&value, cursor);
                        value.drain(cursor..next);
                    }
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
        self.terminal_mut()?
            .draw(|frame| render_loading_prompt(frame, prompt, note))
            .map(|_| ())
            .map_err(|e| miette!("渲染终端 UI 失败: {e}"))
    }

    fn detail(&mut self, prompt: &str, lines: &[String]) -> Result<()> {
        loop {
            self.terminal_mut()?
                .draw(|frame| render_detail_prompt(frame, prompt, lines))
                .map_err(|e| miette!("渲染终端 UI 失败: {e}"))?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled());
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
        let event = event::read().map_err(|e| miette!("读取终端输入失败: {e}"))?;
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                return Ok(key);
            }
        }
    }
}

fn prompt_panel_block() -> Block<'static> {
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
            Span::styled("  inline", Style::default().fg(Color::DarkGray)),
        ]))
}

fn render_select_prompt<T: AsRef<str>>(
    frame: &mut Frame,
    prompt: &str,
    items: &[T],
    state: &mut ListState,
) {
    let block = prompt_panel_block();
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
            Span::styled("Select", Style::default().fg(Color::DarkGray)),
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
            format!("{} option(s)", items.len()),
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
        Paragraph::new("↑↓ / j k move  Enter confirm  Esc cancel")
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_text_prompt(
    frame: &mut Frame,
    prompt: &str,
    value: &str,
    cursor: usize,
    secret: bool,
    error: Option<&str>,
) {
    let block = prompt_panel_block();
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
                if secret { "Secret" } else { "Input" },
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
            "Enter to confirm",
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
            "Value",
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
        Paragraph::new("←→ move  Home/End jump  Backspace/Delete edit  Esc cancel")
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
                "input is masked",
                Style::default().fg(Color::DarkGray),
            )),
            None => Line::from(Span::styled(
                "plain text input",
                Style::default().fg(Color::DarkGray),
            )),
        }),
        note_area,
    );
}

fn render_loading_prompt(frame: &mut Frame, prompt: &str, note: &str) {
    let block = prompt_panel_block();
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
            Span::styled("Loading", Style::default().fg(Color::DarkGray)),
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
            Span::styled("请稍候…", Style::default().fg(Color::White)),
        ])),
        body_area,
    );
    frame.render_widget(
        Paragraph::new("等待远端响应").style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_detail_prompt(frame: &mut Frame, prompt: &str, lines: &[String]) {
    let block = prompt_panel_block();
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
            Span::styled("Detail", Style::default().fg(Color::DarkGray)),
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
            format!("{} line(s)", lines.len()),
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
        Paragraph::new("Enter / Esc back").style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn previous_char_boundary(s: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    s[..index]
        .char_indices()
        .last()
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
// Provider 向导
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    OpenAI,
    GithubCopilot,
    OpenAICompatible,
}

impl ProviderKind {
    const LABELS: &'static [&'static str] = &[
        "OpenAI",
        "GitHub Copilot",
        "OpenAI-compatible（Ollama / LMStudio / 本地）",
    ];

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::OpenAI,
            1 => Self::GithubCopilot,
            _ => Self::OpenAICompatible,
        }
    }
}

/// 交互式填写一个 provider 的凭据，返回 (provider_name, ProviderConfig)
async fn prompt_provider(
    ui: &mut PromptUi,
    existing_names: &[String],
) -> Result<(String, ProviderConfig)> {
    let kind_idx = ui.select("Provider 类型", ProviderKind::LABELS, 0)?;
    let kind = ProviderKind::from_index(kind_idx);

    let default_name = match kind {
        ProviderKind::OpenAI => "openai",
        ProviderKind::GithubCopilot => "copilot",
        ProviderKind::OpenAICompatible => "local",
    };
    // 若名称已存在则加序号避免冲突
    let default_name = if existing_names.contains(&default_name.to_string()) {
        format!("{}-2", default_name)
    } else {
        default_name.to_string()
    };

    let name = ui.text(
        "Provider 名称（在 config.toml 中的 key）",
        Some(&default_name),
    )?;

    let provider = match kind {
        ProviderKind::OpenAI => {
            let api_key = ui.password("OpenAI API key（sk-...）")?;
            let use_custom_url =
                ui.confirm("使用自定义 base URL？（默认 api.openai.com）", false)?;
            let base_url = if use_custom_url {
                let url = ui.text(
                    "Base URL（API 根路径，例如 https://api.openai.com/v1）",
                    None,
                )?;
                Some(normalize_provider_base_url(&url))
            } else {
                None
            };
            ProviderConfig::Openai { api_key, base_url }
        }
        ProviderKind::GithubCopilot => {
            let auth_method = ui.select(
                "GitHub 认证方式",
                &[
                    "Device code 登录（推荐，浏览器授权）",
                    "手动填写 GitHub Token（PAT）",
                    "使用环境变量（GITHUB_TOKEN / GH_TOKEN）",
                ],
                0,
            )?;

            let github_token = match auth_method {
                0 => {
                    ui.suspend();
                    let result = run_github_device_flow().await;
                    ui.resume()?;
                    result?
                }
                1 => ui.password("GitHub Token（ghp_...）")?,
                _ => "${GITHUB_TOKEN}".to_string(),
            };
            ProviderConfig::GithubCopilot { github_token }
        }
        ProviderKind::OpenAICompatible => {
            let base_url = ui.text(
                "Base URL（API 根路径，例如 http://localhost:11434/v1）",
                Some("http://localhost:11434/v1"),
            )?;
            let api_key = ui.text(
                "API key（Ollama 等本地服务可填 ollama 或任意值）",
                Some("ollama"),
            )?;
            ProviderConfig::OpenaiCompatible {
                base_url: normalize_provider_base_url(&base_url),
                api_key,
            }
        }
    };

    Ok((name, provider))
}

// ---------------------------------------------------------------------------
// Model 探测
// ---------------------------------------------------------------------------

/// GitHub Copilot 已知可用模型（openclaw 同款静态列表）
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

/// 展开 `${VAR}` / `$VAR` 形式的环境变量引用，失败时返回空字符串。
fn resolve_token(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(inner).unwrap_or_default();
    }
    if let Some(var) = t.strip_prefix('$') {
        return std::env::var(var).unwrap_or_default();
    }
    t.to_string()
}

/// 模型探测：先试 session token（内部 API，全量模型），失败降级到公共 API，再失败用静态列表。
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

    let token = resolve_token(github_token);
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

/// API 返回的模型信息（context window 和 max tokens 来自响应，非推测）
#[derive(Debug, Clone)]
struct DiscoveredModel {
    id: String,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
}

/// 从 provider 的 models 接口拉取模型列表；失败时返回空 Vec。
async fn fetch_model_ids(provider: &ProviderConfig) -> Vec<DiscoveredModel> {
    match provider {
        ProviderConfig::GithubCopilot { github_token } => fetch_copilot_models(github_token).await,
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            fetch_openai_models(base, api_key).await
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            fetch_openai_models(base_url, api_key).await
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
// Model 向导
// ---------------------------------------------------------------------------

/// 交互式填写一个 model 定义，返回 (model_name, ModelConfig)
async fn prompt_model(
    ui: &mut PromptUi,
    provider_name: &str,
    provider: &ProviderConfig,
) -> Result<(String, ModelConfig)> {
    ui.loading("获取模型列表", &format!("provider: {provider_name}"))?;
    let discovered = fetch_model_ids(provider).await;

    let (model_id, api_ctx, api_out) = if discovered.is_empty() {
        let id = ui.text("Model ID", None)?;
        (id, None, None)
    } else {
        const MANUAL: &str = "手动输入...";
        let labels: Vec<String> = discovered
            .iter()
            .map(|m| m.id.clone())
            .chain(std::iter::once(MANUAL.to_string()))
            .collect();

        let idx = ui.select("选择模型", &labels, 0)?;

        if labels[idx] == MANUAL {
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
        .last()
        .unwrap_or(&model_id)
        .to_string();
    let name = ui.text("Model 名称（config.toml 中的 key）", Some(&default_name))?;

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
// 公开 API
// ---------------------------------------------------------------------------

/// 首次运行时的完整 setup 向导，写入 config.toml 后返回生成的 Config
pub async fn run_first_time_setup() -> Result<Config> {
    println!();
    println!("欢迎使用 Daat Locus");
    println!("未找到配置文件，先来完成初始化设置。");
    println!();

    let mut ui = PromptUi::new()?;
    let skip = ui.select(
        "如何初始化？",
        &[
            "交互式配置（推荐）",
            "跳过，创建默认配置（需手动编辑 config.toml）",
        ],
        0,
    )?;

    if skip == 1 {
        let config = Config::default();
        write_config(&config).await?;
        info("已创建默认配置。请编辑 ~/.daat_locus/config.toml 后重启。");
        return Ok(config);
    }

    // === 配置第一个 Provider ===
    header("步骤 1/2：添加 Provider");
    let (provider_name, provider_config) = prompt_provider(&mut ui, &[]).await?;

    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider_config.clone());

    // === 配置第一个 Model ===
    header("步骤 2/2：添加模型");
    let (model_name, model_config) =
        prompt_model(&mut ui, &provider_name, &provider_config).await?;

    let mut models = HashMap::new();
    models.insert(model_name.clone(), model_config);

    let config = Config {
        providers,
        models,
        main_model: model_name.clone(),
        judge: JudgeConfig::default(),
        ..Config::default()
    };

    write_config(&config).await?;

    println!();
    println!("配置已写入 ~/.daat_locus/config.toml");
    println!("  main_model = \"{model_name}\" （provider: {provider_name}）");
    println!();

    Ok(config)
}

/// `config add-provider` 子命令
pub async fn run_add_provider() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    header("添加 Provider");
    let existing: Vec<String> = config.providers.keys().cloned().collect();
    let mut ui = PromptUi::new()?;
    let (name, provider) = prompt_provider(&mut ui, &existing).await?;

    if config.providers.contains_key(&name) {
        let overwrite = ui.confirm(&format!("provider '{name}' 已存在，是否覆盖？"), false)?;
        if !overwrite {
            info("已取消。");
            return Ok(());
        }
    }

    config.providers.insert(name.clone(), provider);
    write_config(&config).await?;
    info(&format!("Provider '{name}' 已保存。"));
    Ok(())
}

/// `config add-model` 子命令
pub async fn run_add_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    header("添加 Model");
    let mut ui = PromptUi::new()?;
    let provider_names: Vec<String> = config.providers.keys().cloned().collect();
    if provider_names.is_empty() {
        return Err(miette!("没有可用的 provider，请先运行 config add-provider"));
    }
    let provider_idx = if provider_names.len() == 1 {
        0
    } else {
        ui.select("绑定 Provider", &provider_names, 0)?
    };
    let provider_name = &provider_names[provider_idx];
    let provider_config = config.providers.get(provider_name).unwrap();
    let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;

    if config.models.contains_key(&name) {
        let overwrite = ui.confirm(&format!("model '{name}' 已存在，是否覆盖？"), false)?;
        if !overwrite {
            info("已取消。");
            return Ok(());
        }
    }

    config.models.insert(name.clone(), model);

    let set_main = ui.confirm(&format!("将 '{name}' 设为 main_model？"), false)?;
    if set_main {
        config.main_model = name.clone();
    }

    write_config(&config).await?;
    info(&format!("Model '{name}' 已保存。"));
    Ok(())
}

/// `config set-main-model` 子命令
pub async fn run_set_main_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;
    let mut ui = PromptUi::new()?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("没有已配置的 model，请先运行 config add-model"));
    }

    let current_idx = model_names
        .iter()
        .position(|n| n == &config.main_model)
        .unwrap_or(0);

    let idx = ui.select("选择 main_model", &model_names, current_idx)?;

    config.main_model = model_names[idx].clone();
    write_config(&config).await?;
    info(&format!("main_model 已设为 '{}'。", config.main_model));
    Ok(())
}

/// `config show` 子命令：打印当前配置摘要（隐藏 secrets）
pub async fn show_config() -> Result<()> {
    let config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    for line in render_config_summary_lines(&config) {
        println!("{line}");
    }
    println!();
    Ok(())
}

fn render_config_summary_lines(config: &Config) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push("Providers".to_string());
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
    lines.push("Models".to_string());
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
    lines.push("Judge".to_string());
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
    lines.push("Hindsight".to_string());
    lines.push("─────────".to_string());
    let hindsight_model = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    lines.push(format!(
        "  model={}{}  port={}  profile={}",
        hindsight_model,
        if config.hindsight.model.is_none() {
            " (同 main_model)"
        } else {
            ""
        },
        config.hindsight.port,
        config.hindsight.profile,
    ));

    lines
}

/// `config`（无子命令）：交互式菜单，循环直到用户选择退出
pub async fn run_config_menu() -> Result<()> {
    let mut ui = PromptUi::new()?;
    loop {
        // 加载最新 config 用于状态展示（出错时也继续，允许从无 config 状态开始配置）
        let has_config = crate::config::config_file_exists().await;
        let status = if has_config {
            match crate::config::load_config().await {
                Ok(cfg) => format!(
                    "main_model={} | providers={} | models={}",
                    cfg.main_model,
                    cfg.providers.len(),
                    cfg.models.len()
                ),
                Err(e) => format!("配置加载错误: {e}"),
            }
        } else {
            "尚未配置".to_string()
        };

        const ITEMS: &[&str] = &[
            "查看配置详情",
            "添加 Provider",
            "添加 Model",
            "更改 main_model",
            "更改 hindsight 模型",
            "退出",
        ];

        let idx = ui.select(&format!("配置管理  {status}"), ITEMS, 0)?;

        match idx {
            0 => match crate::config::load_config().await {
                Ok(cfg) => ui.detail("配置详情", &render_config_summary_lines(&cfg))?,
                Err(e) => ui.detail("配置详情", &[format!("加载配置失败: {e}")])?,
            },
            1 => {
                let mut config = crate::config::load_config()
                    .await
                    .map_err(|e| miette!("加载配置失败: {e}"))?;
                let existing: Vec<String> = config.providers.keys().cloned().collect();
                let (name, provider) = prompt_provider(&mut ui, &existing).await?;
                if config.providers.contains_key(&name)
                    && !ui.confirm(&format!("provider '{name}' 已存在，是否覆盖？"), false)?
                {
                    continue;
                }
                config.providers.insert(name, provider);
                write_config(&config).await?;
            }
            2 => {
                let mut config = crate::config::load_config()
                    .await
                    .map_err(|e| miette!("加载配置失败: {e}"))?;
                let provider_names: Vec<String> = config.providers.keys().cloned().collect();
                if provider_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("没有可用的 provider，请先运行 config add-provider"));
                }
                let provider_idx = if provider_names.len() == 1 {
                    0
                } else {
                    ui.select("绑定 Provider", &provider_names, 0)?
                };
                let provider_name = &provider_names[provider_idx];
                let provider_config = config.providers.get(provider_name).unwrap();
                let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;
                if config.models.contains_key(&name)
                    && !ui.confirm(&format!("model '{name}' 已存在，是否覆盖？"), false)?
                {
                    continue;
                }
                config.models.insert(name.clone(), model);
                if ui.confirm(&format!("将 '{name}' 设为 main_model？"), false)? {
                    config.main_model = name;
                }
                write_config(&config).await?;
            }
            3 => {
                let mut config = crate::config::load_config()
                    .await
                    .map_err(|e| miette!("加载配置失败: {e}"))?;
                let model_names: Vec<String> = config.models.keys().cloned().collect();
                if model_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("没有已配置的 model，请先运行 config add-model"));
                }
                let current_idx = model_names
                    .iter()
                    .position(|n| n == &config.main_model)
                    .unwrap_or(0);
                let idx = ui.select("选择 main_model", &model_names, current_idx)?;
                config.main_model = model_names[idx].clone();
                write_config(&config).await?;
            }
            4 => {
                let mut config = crate::config::load_config()
                    .await
                    .map_err(|e| miette!("加载配置失败: {e}"))?;
                let model_names: Vec<String> = config.models.keys().cloned().collect();
                if model_names.is_empty() {
                    ui.suspend();
                    return Err(miette!("没有已配置的 model，请先运行 config add-model"));
                }
                const USE_MAIN: &str = "与 main_model 相同（默认）";
                let mut items: Vec<String> = model_names.clone();
                items.push(USE_MAIN.to_string());
                let current_idx = config
                    .hindsight
                    .model
                    .as_ref()
                    .and_then(|m| model_names.iter().position(|n| n == m))
                    .unwrap_or(items.len() - 1);
                let idx = ui.select("选择 hindsight 使用的模型", &items, current_idx)?;
                config.hindsight.model = if items[idx] == USE_MAIN {
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

/// `config set-hindsight-model` 子命令
pub async fn run_set_hindsight_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;
    let mut ui = PromptUi::new()?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("没有已配置的 model，请先运行 config add-model"));
    }

    const USE_MAIN: &str = "与 main_model 相同（默认）";
    let mut items: Vec<String> = model_names.clone();
    items.push(USE_MAIN.to_string());

    let current_idx = config
        .hindsight
        .model
        .as_ref()
        .and_then(|m| model_names.iter().position(|n| n == m))
        .unwrap_or(items.len() - 1); // 默认选最后一项（USE_MAIN）

    let idx = ui.select("选择 hindsight 使用的模型", &items, current_idx)?;

    config.hindsight.model = if items[idx] == USE_MAIN {
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
    info(&format!("hindsight 模型已设为 '{display}'。"));
    Ok(())
}

fn mask_secret(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    // 显示前4位和最后4位
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
