use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use octocrab::models::issues::Comment as ApiComment;
use rat_cursor::HasScreenCursor;
use rat_widget::{
    event::{HandleEvent, ct_event},
    focus::{FocusBuilder, FocusFlag, HasFocus, Navigation},
    list::{ListState, selection::RowSelection},
    textarea::{TextArea, TextAreaState, TextWrap},
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout as TuiLayout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, ListItem, StatefulWidget},
};
use ratatui_macros::line;
use textwrap::{Options, wrap};
use throbber_widgets_tui::{BRAILLE_SIX_DOUBLE, Throbber, ThrobberState, WhichUse};

use crate::{
    app::GITHUB_CLIENT,
    ui::{
        Action,
        components::{Component, issue_list::MainScreen},
        layout::Layout,
        utils::get_border_style,
    },
};

#[derive(Debug, Clone)]
pub struct IssueConversationSeed {
    pub number: u64,
    pub author: Arc<str>,
    pub created_at: Arc<str>,
    pub body: Option<Arc<str>>,
}

impl IssueConversationSeed {
    pub fn from_issue(issue: &octocrab::models::issues::Issue) -> Self {
        Self {
            number: issue.number,
            author: Arc::<str>::from(issue.user.login.as_str()),
            created_at: Arc::<str>::from(issue.created_at.format("%Y-%m-%d %H:%M").to_string()),
            body: issue.body.as_ref().map(|b| Arc::<str>::from(b.as_str())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommentView {
    pub id: u64,
    pub author: Arc<str>,
    pub created_at: Arc<str>,
    pub body: Arc<str>,
}

impl CommentView {
    pub fn from_api(comment: ApiComment) -> Self {
        let body = comment.body.unwrap_or_else(|| "".to_string());
        Self {
            id: comment.id.0,
            author: Arc::<str>::from(comment.user.login.as_str()),
            created_at: Arc::<str>::from(comment.created_at.format("%Y-%m-%d %H:%M").to_string()),
            body: Arc::<str>::from(body),
        }
    }
}

pub struct IssueConversation {
    action_tx: Option<tokio::sync::mpsc::Sender<Action>>,
    current: Option<IssueConversationSeed>,
    cache: HashMap<u64, Vec<CommentView>>,
    loading: HashSet<u64>,
    posting: bool,
    error: Option<String>,
    post_error: Option<String>,
    owner: String,
    repo: String,
    current_user: String,
    list_state: ListState<RowSelection>,
    input_state: TextAreaState,
    throbber_state: ThrobberState,
    post_throbber_state: ThrobberState,
    screen: MainScreen,
    focus: FocusFlag,
    area: Rect,
}

impl IssueConversation {
    pub fn new(app_state: crate::ui::AppState) -> Self {
        Self {
            action_tx: None,
            current: None,
            cache: HashMap::new(),
            loading: HashSet::new(),
            posting: false,
            error: None,
            post_error: None,
            owner: app_state.owner,
            repo: app_state.repo,
            current_user: app_state.current_user,
            list_state: ListState::default(),
            input_state: TextAreaState::new(),
            throbber_state: ThrobberState::default(),
            post_throbber_state: ThrobberState::default(),
            screen: MainScreen::default(),
            focus: FocusFlag::new().with_name("issue_conversation"),
            area: Rect::default(),
        }
    }

    pub fn render(&mut self, area: Layout, buf: &mut Buffer) {
        self.area = area.main_content;
        let areas = TuiLayout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(5)])
            .split(area.main_content);
        let content_area = areas[0];
        let input_area = areas[1];

        let items = self.build_items(content_area);
        let mut list_block = Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(get_border_style(&self.list_state));

        if self.is_loading_current() {
            let title_area = Rect {
                x: content_area.x + 1,
                y: content_area.y,
                width: 10,
                height: 1,
            };
            let throbber = Throbber::default()
                .label("Loading")
                .style(Style::new().fg(Color::Cyan))
                .throbber_set(BRAILLE_SIX_DOUBLE)
                .use_type(WhichUse::Spin);
            StatefulWidget::render(throbber, title_area, buf, &mut self.throbber_state);
        } else {
            list_block = list_block.title("Conversation");
        }

        let list = rat_widget::list::List::<RowSelection>::new(items)
            .block(list_block)
            .style(Style::default())
            .focus_style(Style::default().bold().reversed())
            .select_style(Style::default().add_modifier(Modifier::BOLD));
        list.render(content_area, buf, &mut self.list_state);

        let input_title = if let Some(err) = &self.post_error {
            format!("Comment (Ctrl+Enter to send) | {err}")
        } else {
            "Comment (Ctrl+Enter to send)".to_string()
        };
        let input_block = Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(get_border_style(&self.input_state))
            .title(input_title);
        let input_widget = TextArea::new()
            .block(input_block)
            .text_wrap(TextWrap::Word(4));
        input_widget.render(input_area, buf, &mut self.input_state);

        if self.posting {
            let title_area = Rect {
                x: input_area.x + 1,
                y: input_area.y,
                width: 10,
                height: 1,
            };
            let throbber = Throbber::default()
                .label("Sending")
                .style(Style::new().fg(Color::Cyan))
                .throbber_set(BRAILLE_SIX_DOUBLE)
                .use_type(WhichUse::Spin);
            StatefulWidget::render(throbber, title_area, buf, &mut self.post_throbber_state);
        }
    }

    fn build_items(&self, content_area: Rect) -> Vec<ListItem<'static>> {
        let mut items = Vec::new();
        let width = content_area.width.saturating_sub(4).max(10) as usize;

        if let Some(err) = &self.error {
            items.push(ListItem::new(line![Span::styled(
                err.clone(),
                Style::new().fg(Color::Red)
            )]));
        }

        let Some(seed) = &self.current else {
            items.push(ListItem::new(line![Span::styled(
                "Press Enter on an issue to view the conversation.".to_string(),
                Style::new().dim()
            )]));
            return items;
        };

        if let Some(body) = seed
            .body
            .as_ref()
            .map(|b| b.as_ref())
            .filter(|b| !b.trim().is_empty())
        {
            items.push(build_comment_item(
                seed.author.as_ref().to_string(),
                seed.created_at.as_ref().to_string(),
                body.to_string(),
                width,
                seed.author.as_ref() == self.current_user,
            ));
        }

        if let Some(comments) = self.cache.get(&seed.number) {
            for comment in comments {
                items.push(build_comment_item(
                    comment.author.as_ref().to_string(),
                    comment.created_at.as_ref().to_string(),
                    comment.body.as_ref().to_string(),
                    width,
                    comment.author.as_ref() == self.current_user,
                ));
            }
        }

        items
    }

    fn is_loading_current(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|seed| self.loading.contains(&seed.number))
    }

    async fn fetch_comments(&mut self, number: u64) {
        if self.loading.contains(&number) {
            return;
        }
        let Some(action_tx) = self.action_tx.clone() else {
            return;
        };
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        self.loading.insert(number);
        self.error = None;

        tokio::spawn(async move {
            let Some(client) = GITHUB_CLIENT.get() else {
                let _ = action_tx
                    .send(Action::IssueCommentsError {
                        number,
                        message: "GitHub client not initialized.".to_string(),
                    })
                    .await;
                return;
            };
            let handler = client.inner().issues(owner, repo);
            let page = handler
                .list_comments(number)
                .per_page(100u8)
                .page(1u32)
                .send()
                .await;

            match page {
                Ok(mut p) => {
                    let comments = std::mem::take(&mut p.items)
                        .into_iter()
                        .map(CommentView::from_api)
                        .collect();
                    let _ = action_tx
                        .send(Action::IssueCommentsLoaded { number, comments })
                        .await;
                }
                Err(err) => {
                    let _ = action_tx
                        .send(Action::IssueCommentsError {
                            number,
                            message: err.to_string().replace('\n', " "),
                        })
                        .await;
                }
            }
        });
    }

    async fn send_comment(&mut self, number: u64, body: String) {
        let Some(action_tx) = self.action_tx.clone() else {
            return;
        };
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        self.posting = true;
        self.post_error = None;

        tokio::spawn(async move {
            let Some(client) = GITHUB_CLIENT.get() else {
                let _ = action_tx
                    .send(Action::IssueCommentPostError {
                        number,
                        message: "GitHub client not initialized.".to_string(),
                    })
                    .await;
                return;
            };
            let handler = client.inner().issues(owner, repo);
            match handler.create_comment(number, body).await {
                Ok(comment) => {
                    let _ = action_tx
                        .send(Action::IssueCommentPosted {
                            number,
                            comment: CommentView::from_api(comment),
                        })
                        .await;
                }
                Err(err) => {
                    let _ = action_tx
                        .send(Action::IssueCommentPostError {
                            number,
                            message: err.to_string().replace('\n', " "),
                        })
                        .await;
                }
            }
        });
    }
}

#[async_trait(?Send)]
impl Component for IssueConversation {
    fn render(&mut self, area: Layout, buf: &mut Buffer) {
        self.render(area, buf);
    }

    fn register_action_tx(&mut self, action_tx: tokio::sync::mpsc::Sender<Action>) {
        self.action_tx = Some(action_tx);
    }

    async fn handle_event(&mut self, event: Action) {
        match event {
            Action::AppEvent(ref event) => {
                if self.screen != MainScreen::Details {
                    return;
                }
                if matches!(event, ct_event!(keycode press Tab)) && self.input_state.is_focused() {
                    self.action_tx
                        .as_ref()
                        .unwrap()
                        .send(Action::ForceFocusChange)
                        .await
                        .unwrap();
                }
                if let crossterm::event::Event::Key(key) = event {
                    if key.code == crossterm::event::KeyCode::Esc {
                        if let Some(tx) = self.action_tx.clone() {
                            let _ = tx.send(Action::ChangeIssueScreen(MainScreen::List)).await;
                        }
                        return;
                    }
                    if key.code == crossterm::event::KeyCode::Enter
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        let Some(seed) = &self.current else {
                            return;
                        };
                        let body = self.input_state.text();
                        let trimmed = body.trim();
                        if trimmed.is_empty() {
                            self.post_error = Some("Comment cannot be empty.".to_string());
                            return;
                        }
                        self.input_state.set_text("");
                        self.send_comment(seed.number, trimmed.to_string()).await;
                        return;
                    }
                }
                self.list_state.handle(event, rat_widget::event::Regular);
                if !matches!(event, ct_event!(keycode press Tab)) {
                    self.input_state.handle(event, rat_widget::event::Regular);
                }
            }
            Action::EnterIssueDetails { seed } => {
                let number = seed.number;
                self.current = Some(seed);
                self.post_error = None;
                if self.cache.contains_key(&number) {
                    self.loading.remove(&number);
                    self.error = None;
                } else {
                    self.fetch_comments(number).await;
                }
            }
            Action::IssueCommentsLoaded { number, comments } => {
                self.cache.insert(number, comments);
                self.loading.remove(&number);
                if self.current.as_ref().is_some_and(|s| s.number == number) {
                    self.error = None;
                }
            }
            Action::IssueCommentPosted { number, comment } => {
                self.posting = false;
                if let Some(list) = self.cache.get_mut(&number) {
                    list.push(comment);
                } else {
                    self.cache.insert(number, vec![comment]);
                }
            }
            Action::IssueCommentsError { number, message } => {
                self.loading.remove(&number);
                if self.current.as_ref().is_some_and(|s| s.number == number) {
                    self.error = Some(message);
                }
            }
            Action::IssueCommentPostError { number, message } => {
                self.posting = false;
                if self.current.as_ref().is_some_and(|s| s.number == number) {
                    self.post_error = Some(message);
                }
            }
            Action::ChangeIssueScreen(screen) => {
                self.screen = screen;
                match screen {
                    MainScreen::List => {
                        self.input_state.focus.set(false);
                        self.list_state.focus.set(false);
                    }
                    MainScreen::Details => {}
                }
            }
            Action::Tick => {
                if self.is_loading_current() {
                    self.throbber_state.calc_next();
                }
                if self.posting {
                    self.post_throbber_state.calc_next();
                }
            }
            _ => {}
        }
    }

    fn cursor(&self) -> Option<(u16, u16)> {
        self.input_state.screen_cursor()
    }

    fn should_render(&self) -> bool {
        self.screen == MainScreen::Details
    }

    fn capture_focus_event(&self, event: &crossterm::event::Event) -> bool {
        if self.screen != MainScreen::Details {
            return false;
        }
        if !self.input_state.is_focused() {
            return false;
        }
        match event {
            crossterm::event::Event::Key(key) => matches!(
                key.code,
                crossterm::event::KeyCode::Tab | crossterm::event::KeyCode::BackTab
            ),
            _ => false,
        }
    }
}

impl HasFocus for IssueConversation {
    fn build(&self, builder: &mut FocusBuilder) {
        let tag = builder.start(self);
        builder.widget(&self.list_state);
        builder.widget(&self.input_state);
        builder.end(tag);
    }

    fn focus(&self) -> FocusFlag {
        self.focus.clone()
    }

    fn area(&self) -> Rect {
        self.area
    }

    fn navigable(&self) -> Navigation {
        if self.screen == MainScreen::Details {
            Navigation::Regular
        } else {
            Navigation::None
        }
    }
}

fn build_comment_item(
    author: String,
    created_at: String,
    body: String,
    width: usize,
    is_self: bool,
) -> ListItem<'static> {
    let author_style = if is_self {
        Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::Cyan)
    };
    let header = Line::from(vec![
        Span::styled(author, author_style),
        Span::raw("  "),
        Span::styled(created_at, Style::new().dim()),
    ]);
    let mut lines = vec![header];
    let wrap_opts = Options::new(width).break_words(false);
    let wrapped = wrap(&body, wrap_opts);
    for line in wrapped {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::raw(line.into_owned()),
        ]));
    }
    ListItem::new(lines)
}
