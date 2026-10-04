use axum::{
    Router,
    body::Bytes,
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use rand::RngCore;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, env, path::PathBuf, sync::Arc, time::Duration};
use tokio::time::sleep;

#[derive(Clone)]
struct Target {
    platform: String,
    id: String,
    label: String,
}

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    password: String,
    telegram_token: String,
    slack_token: String,
    timezone: Tz,
    targets: HashMap<String, Target>,
    client: reqwest::Client,
    secure_cookie: bool,
}

#[derive(Clone)]
struct Post {
    title: String,
    body: String,
    link: String,
    image_url: String,
    due_at: i64,
}

#[derive(Clone)]
struct Job {
    id: i64,
    target_key: String,
    post: Post,
}

fn now() -> i64 {
    Utc::now().timestamp()
}
fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn constant_eq(a: &str, b: &str) -> bool {
    let left = Sha256::digest(a.as_bytes());
    let right = Sha256::digest(b.as_bytes());
    let mut diff = 0u8;
    for (x, y) in left.iter().zip(right.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
fn connection(state: &AppState) -> Result<Connection, String> {
    let con = Connection::open(&state.db_path).map_err(|e| e.to_string())?;
    con.busy_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    con.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .map_err(|e| e.to_string())?;
    Ok(con)
}
fn init_db(state: &AppState) -> Result<(), String> {
    if let Some(parent) = state.db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let con = connection(state)?;
    con.execute_batch("\
        CREATE TABLE IF NOT EXISTS sessions(token_hash TEXT PRIMARY KEY, csrf TEXT, expires INTEGER);\
        CREATE TABLE IF NOT EXISTS posts(id INTEGER PRIMARY KEY, title TEXT, body TEXT NOT NULL, link TEXT, image_url TEXT, due_at INTEGER, created_at INTEGER);\
        CREATE TABLE IF NOT EXISTS deliveries(id INTEGER PRIMARY KEY, post_id INTEGER REFERENCES posts(id), target_key TEXT, status TEXT, attempts INTEGER DEFAULT 0, message_ref TEXT, error TEXT, UNIQUE(post_id,target_key));\
        UPDATE deliveries SET status='failed', error='服务中断时正在发送；请先检查频道，再决定是否重试' WHERE status='publishing';")
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn parse_targets() -> HashMap<String, Target> {
    let mut targets = HashMap::new();
    for (platform, variable) in [
        ("telegram", "TELEGRAM_CHANNELS"),
        ("slack", "SLACK_CHANNELS"),
    ] {
        for entry in env::var(variable)
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
        {
            let (id, label) = entry
                .trim()
                .split_once('|')
                .unwrap_or((entry.trim(), entry.trim()));
            targets.insert(
                format!("{platform}:{id}"),
                Target {
                    platform: platform.into(),
                    id: id.into(),
                    label: label.into(),
                },
            );
        }
    }
    targets
}
fn html_page(body: &str) -> Response {
    let markup = format!(
        "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>频道发布台</title><style>{}</style></head><body><header><h1>频道发布台</h1><p>一次编辑，发布到 Telegram 和 Slack</p></header>{body}</body></html>",
        include_str!("style.css")
    );
    let mut response = Html(markup).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'",
        ),
    );
    response
}
fn page_error(message: &str, status: StatusCode) -> Response {
    let mut response = html_page(&format!(
        "<main><section class=\"card\"><p class=\"notice error\">{}</p><a href=\"/\">返回</a></section></main>",
        escape(message)
    ));
    *response.status_mut() = status;
    response
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}
fn session(state: &AppState, headers: &HeaderMap) -> Option<(String, String)> {
    let token = cookie(headers, "session")?;
    let con = connection(state).ok()?;
    let csrf: String = con
        .query_row(
            "SELECT csrf FROM sessions WHERE token_hash=?1 AND expires>?2",
            params![hash(&token), now()],
            |row| row.get(0),
        )
        .ok()?;
    Some((token, csrf))
}
fn form(body: Bytes) -> Result<HashMap<String, Vec<String>>, String> {
    if body.len() > 100_000 {
        return Err("表单过大".into());
    }
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_bytes(&body).map_err(|e| e.to_string())?;
    let mut values: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in pairs {
        values.entry(key).or_default().push(value);
    }
    Ok(values)
}
fn first<'a>(values: &'a HashMap<String, Vec<String>>, key: &str) -> &'a str {
    values
        .get(key)
        .and_then(|items| items.first())
        .map(String::as_str)
        .unwrap_or("")
}
fn csrf_valid(values: &HashMap<String, Vec<String>>, csrf: &str) -> bool {
    constant_eq(first(values, "csrf"), csrf)
}
fn date_label(timestamp: i64, tz: Tz) -> String {
    tz.timestamp_opt(timestamp, 0)
        .single()
        .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}
fn validate_post(
    state: &AppState,
    values: &HashMap<String, Vec<String>>,
) -> Result<(Post, Vec<String>, String), String> {
    let title = first(values, "title").trim().to_string();
    let body = first(values, "body").trim().to_string();
    let link = first(values, "link").trim().to_string();
    let image_url = first(values, "image_url").trim().to_string();
    let selected = values.get("target").cloned().unwrap_or_default();
    let mut unique = Vec::new();
    for key in selected {
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    if body.is_empty()
        || unique.is_empty()
        || unique.iter().any(|key| !state.targets.contains_key(key))
    {
        return Err("请填写正文并选择至少一个有效频道".into());
    }
    for (value, max) in [
        (&title, 200),
        (&body, 3500),
        (&link, 1000),
        (&image_url, 1000),
    ] {
        if value.chars().count() > max {
            return Err("内容过长".into());
        }
    }
    for value in [&link, &image_url] {
        if !value.is_empty() {
            let url = url::Url::parse(value).map_err(|_| "链接无效")?;
            if !["http", "https"].contains(&url.scheme()) {
                return Err("链接必须以 http:// 或 https:// 开头".into());
            }
        }
    }
    let scheduled = first(values, "scheduled").to_string();
    let due_at = if scheduled.is_empty() {
        now()
    } else {
        let naive = NaiveDateTime::parse_from_str(&scheduled, "%Y-%m-%dT%H:%M")
            .map_err(|_| "定时时间无效")?;
        match state.timezone.from_local_datetime(&naive) {
            LocalResult::Single(date) => date.timestamp(),
            _ => return Err("定时时间在当前时区不存在或不明确".into()),
        }
    };
    Ok((
        Post {
            title,
            body,
            link,
            image_url,
            due_at,
        },
        unique,
        scheduled,
    ))
}

async fn send_api(
    state: &AppState,
    url: &str,
    payload: Value,
    token: Option<&str>,
) -> Result<Value, String> {
    let mut request = state.client.post(url).json(&payload);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    // Telegram includes the bot token in the request URL. Never expose a
    // reqwest error here: it may include that URL in the UI and database.
    let response = request
        .send()
        .await
        .map_err(|_| "连接失败，请检查网络或 Bot 凭证".to_string())?;
    let status = response.status();
    let result: Value = response
        .json()
        .await
        .map_err(|_| "平台响应解析失败".to_string())?;
    if !status.is_success() || result.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(result
            .get("description")
            .or_else(|| result.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}")));
    }
    Ok(result)
}
async fn publish(state: &AppState, post: &Post, target: &Target) -> Result<String, String> {
    let joined = [&post.title, &post.body, &post.link]
        .into_iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if target.platform == "telegram" {
        if state.telegram_token.is_empty() {
            return Err("未配置 TELEGRAM_BOT_TOKEN".into());
        }
        if joined.chars().count() > 4096 {
            return Err("Telegram 文字超过 4096 字".into());
        }
        let (method, payload) = if post.image_url.is_empty() {
            ("sendMessage", json!({"chat_id": target.id, "text": joined}))
        } else {
            if joined.chars().count() > 1024 {
                return Err("Telegram 图片说明超过 1024 字，请缩短内容".into());
            }
            (
                "sendPhoto",
                json!({"chat_id": target.id, "photo": post.image_url, "caption": joined}),
            )
        };
        let url = format!(
            "https://api.telegram.org/bot{}/{method}",
            state.telegram_token
        );
        let result = send_api(state, &url, payload, None).await?;
        return result
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .ok_or("Telegram 未返回消息 ID".into());
    }
    if state.slack_token.is_empty() {
        return Err("未配置 SLACK_BOT_TOKEN".into());
    }
    let title = if post.title.is_empty() {
        String::new()
    } else {
        format!("*{}*", post.title)
    };
    let text = [&title, &post.body, &post.link]
        .into_iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut payload = json!({"channel": target.id, "text": text});
    if !post.image_url.is_empty() {
        if text.chars().count() > 3000 {
            return Err("Slack 图片消息正文超过 3000 字".into());
        }
        payload["blocks"] = json!([
            {"type": "section", "text": {"type": "mrkdwn", "text": text}},
            {"type": "image", "image_url": post.image_url, "alt_text": if post.title.is_empty() { "发布图片" } else { &post.title }}
        ]);
    }
    let result = send_api(
        state,
        "https://slack.com/api/chat.postMessage",
        payload,
        Some(&state.slack_token),
    )
    .await?;
    result
        .get("ts")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or("Slack 未返回消息时间戳".into())
}
async fn process_due(state: &AppState) -> Result<(), String> {
    let jobs = {
        let con = connection(state)?;
        let mut stmt = con.prepare("SELECT d.id,d.target_key,p.title,p.body,p.link,p.image_url,p.due_at FROM deliveries d JOIN posts p ON p.id=d.post_id WHERE d.status='queued' AND p.due_at<=?1 ORDER BY d.id").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([now()], |row| {
                Ok(Job {
                    id: row.get(0)?,
                    target_key: row.get(1)?,
                    post: Post {
                        title: row.get(2)?,
                        body: row.get(3)?,
                        link: row.get(4)?,
                        image_url: row.get(5)?,
                        due_at: row.get(6)?,
                    },
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for job in jobs {
        let claimed = connection(state)?.execute("UPDATE deliveries SET status='publishing',attempts=attempts+1 WHERE id=?1 AND status='queued'", [job.id]).map_err(|e| e.to_string())?;
        if claimed == 0 {
            continue;
        }
        let result = match state.targets.get(&job.target_key) {
            Some(target) => publish(state, &job.post, target).await,
            None => Err("频道配置已移除".into()),
        };
        let con = connection(state)?;
        match result {
            Ok(reference) => {
                con.execute(
                    "UPDATE deliveries SET status='sent',message_ref=?1,error=NULL WHERE id=?2",
                    params![reference, job.id],
                )
                .map_err(|e| e.to_string())?;
            }
            Err(error) => {
                con.execute(
                    "UPDATE deliveries SET status='failed',error=?1 WHERE id=?2",
                    params![error.chars().take(400).collect::<String>(), job.id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
fn dashboard_html(state: &AppState, csrf: &str, query: &str) -> Result<String, String> {
    let notice = if query.contains("saved=1") {
        "<p class=\"notice\">发布任务已保存。</p>"
    } else if query.contains("retried=1") {
        "<p class=\"notice\">已重新尝试。</p>"
    } else {
        ""
    };
    let mut keys = state.targets.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let mut choices = String::new();
    for key in keys {
        let target = &state.targets[&key];
        choices.push_str(&format!("<label class=\"target\"><input type=\"checkbox\" name=\"target\" value=\"{}\"><span class=\"tag\">{}</span> {}</label>", escape(&key), escape(&target.platform), escape(&target.label)));
    }
    if choices.is_empty() {
        choices = "<p class=\"notice error\">尚未配置频道，请先设置 TELEGRAM_CHANNELS 或 SLACK_CHANNELS。</p>".into();
    }
    let con = connection(state)?;
    let mut posts_stmt = con
        .prepare("SELECT id,title,body,link,image_url,due_at FROM posts ORDER BY id DESC LIMIT 30")
        .map_err(|e| e.to_string())?;
    let posts = posts_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Post {
                    title: row.get(1)?,
                    body: row.get(2)?,
                    link: row.get(3)?,
                    image_url: row.get(4)?,
                    due_at: row.get(5)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut history = String::new();
    for (id, post) in posts {
        let title = if post.title.is_empty() {
            "未命名发布"
        } else {
            &post.title
        };
        history.push_str(&format!("<article class=\"post\"><h3>{}</h3><div class=\"meta\">计划时间 {} · #{id}</div><div class=\"excerpt\">{}</div>", escape(title), date_label(post.due_at, state.timezone), escape(&post.body)));
        let mut deliveries_stmt = con
            .prepare(
                "SELECT id,target_key,status,error FROM deliveries WHERE post_id=?1 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let deliveries = deliveries_stmt
            .query_map([id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for delivery in deliveries {
            let (delivery_id, key, status, error) = delivery.map_err(|e| e.to_string())?;
            let status_name = match status.as_str() {
                "sent" => "已发布",
                "failed" => "失败",
                "queued" => "待发布",
                "publishing" => "发送中",
                _ => "未知",
            };
            let label = state
                .targets
                .get(&key)
                .map(|target| target.label.as_str())
                .unwrap_or(&key);
            let retry = if status == "failed" {
                format!(
                    "<form style=\"display:inline\" method=\"post\" action=\"/retry\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"delivery_id\" value=\"{delivery_id}\"><button class=\"small\">重试</button></form>",
                    escape(csrf)
                )
            } else {
                String::new()
            };
            history.push_str(&format!(
                "<div class=\"delivery\"><b class=\"{}\">{status_name}</b> · {}{}{retry}</div>",
                escape(&status),
                escape(label),
                error
                    .map(|message| format!(" · {}", escape(&message)))
                    .unwrap_or_default()
            ));
        }
        history.push_str("</article>");
    }
    if history.is_empty() {
        history = "<p class=\"hint\">还没有发布记录。</p>".into();
    }
    let form = format!(
        r#"<section class="card"><h2>新建发布</h2><form method="post" action="/preview"><input type="hidden" name="csrf" value="{}"><label>标题</label><input name="title" maxlength="200" placeholder="例如：本周更新"><label>正文 *</label><textarea name="body" maxlength="3500" required placeholder="输入要发布的内容…"></textarea><label>链接</label><input type="url" name="link" placeholder="https://…"><label>图片网址</label><input type="url" name="image_url" placeholder="https://…"><p class="hint">图片需可公开访问。Telegram 图片说明最多 1024 字。</p><label>发布到 *</label>{choices}<label>定时发布</label><input type="datetime-local" name="scheduled"><p class="hint">留空则立即发布；时间使用 {}。</p><button>预览</button></form></section>"#,
        escape(csrf),
        escape(&state.timezone.to_string())
    );
    let sidebar = format!(
        "<section class=\"card\"><h2>发布记录</h2>{history}<form method=\"post\" action=\"/logout\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"small\">退出登录</button></form></section>",
        escape(csrf)
    );
    Ok(format!("<main>{form}<div>{notice}{sidebar}</div></main>"))
}

async fn login_page() -> Response {
    html_page(
        "<main style=\"display:block;max-width:460px\"><section class=\"card\"><h2>管理员登录</h2><form method=\"post\" action=\"/login\"><label>密码</label><input type=\"password\" name=\"password\" required><button>登录</button></form></section></main>",
    )
}
async fn login(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let values = match form(body) {
        Ok(values) => values,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    if !constant_eq(first(&values, "password"), &state.password) {
        return page_error("密码错误", StatusCode::UNAUTHORIZED);
    }
    let token = random_token();
    let csrf = random_token();
    match connection(&state).and_then(|con| {
        con.execute(
            "INSERT INTO sessions VALUES (?1,?2,?3)",
            params![hash(&token), csrf, now() + 86400],
        )
        .map_err(|e| e.to_string())
    }) {
        Ok(_) => {
            let mut response = Redirect::to("/").into_response();
            let secure = if state.secure_cookie { "; Secure" } else { "" };
            let cookie = format!(
                "session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=86400{secure}"
            );
            response
                .headers_mut()
                .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
            response
        }
        Err(error) => page_error(&error, StatusCode::INTERNAL_SERVER_ERROR),
    }
}
async fn dashboard(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let Some((_, csrf)) = session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    match dashboard_html(&state, &csrf, query.as_deref().unwrap_or("")) {
        Ok(body) => html_page(&body),
        Err(error) => page_error(&error, StatusCode::INTERNAL_SERVER_ERROR),
    }
}
async fn preview(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some((_, csrf)) = session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    let values = match form(body) {
        Ok(values) => values,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    if !csrf_valid(&values, &csrf) {
        return page_error("表单验证失败，请刷新页面", StatusCode::BAD_REQUEST);
    }
    let (post, targets, scheduled) = match validate_post(&state, &values) {
        Ok(item) => item,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    let mut hidden = String::new();
    for (key, value) in [
        ("csrf", csrf.as_str()),
        ("title", &post.title),
        ("body", &post.body),
        ("link", &post.link),
        ("image_url", &post.image_url),
        ("scheduled", &scheduled),
    ] {
        hidden.push_str(&format!(
            "<input type=\"hidden\" name=\"{key}\" value=\"{}\">",
            escape(value)
        ));
    }
    let mut destination_labels = Vec::new();
    for key in &targets {
        hidden.push_str(&format!(
            "<input type=\"hidden\" name=\"target\" value=\"{}\">",
            escape(key)
        ));
        destination_labels.push(escape(&state.targets[key].label));
    }
    let mut sections = String::new();
    for platform in ["telegram", "slack"] {
        if targets
            .iter()
            .any(|key| state.targets[key].platform == platform)
        {
            let text = [&post.title, &post.body, &post.link]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n");
            sections.push_str(&format!("<div class=\"post\"><h3>{platform} 预览</h3><div class=\"excerpt\" style=\"max-height:none\">{}</div></div>", escape(&text)));
        }
    }
    let image = if post.image_url.is_empty() {
        String::new()
    } else {
        format!("<p class=\"hint\">图片：{}</p>", escape(&post.image_url))
    };
    let page = format!(
        "<main style=\"display:block;max-width:760px\"><section class=\"card\"><h2>发布预览</h2><p class=\"meta\">目标：{} · 时间：{} ({})</p>{sections}{image}<form method=\"post\" action=\"/create\">{hidden}<button>确认发布</button></form><p><a href=\"/\">返回修改</a></p></section></main>",
        destination_labels.join(", "),
        date_label(post.due_at, state.timezone),
        escape(&state.timezone.to_string())
    );
    html_page(&page)
}
async fn create(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some((_, csrf)) = session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    let values = match form(body) {
        Ok(values) => values,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    if !csrf_valid(&values, &csrf) {
        return page_error("表单验证失败，请刷新页面", StatusCode::BAD_REQUEST);
    }
    let (post, targets, _) = match validate_post(&state, &values) {
        Ok(item) => item,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    let result = (|| -> Result<(), String> {
        let mut con = connection(&state)?;
        let tx = con.transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO posts(title,body,link,image_url,due_at,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![post.title, post.body, post.link, post.image_url, post.due_at, now()]).map_err(|e| e.to_string())?;
        let id = tx.last_insert_rowid();
        for key in targets {
            tx.execute(
                "INSERT INTO deliveries(post_id,target_key,status) VALUES (?1,?2,'queued')",
                params![id, key],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = result {
        return page_error(&error, StatusCode::INTERNAL_SERVER_ERROR);
    }
    if post.due_at <= now() {
        let _ = process_due(&state).await;
    }
    Redirect::to("/?saved=1").into_response()
}
async fn retry(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some((_, csrf)) = session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    let values = match form(body) {
        Ok(values) => values,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    if !csrf_valid(&values, &csrf) {
        return page_error("表单验证失败，请刷新页面", StatusCode::BAD_REQUEST);
    }
    let id: i64 = match first(&values, "delivery_id").parse() {
        Ok(id) => id,
        Err(_) => return page_error("发布记录无效", StatusCode::BAD_REQUEST),
    };
    let result = connection(&state).and_then(|con| {
        con.execute(
            "UPDATE deliveries SET status='queued',error=NULL WHERE id=?1 AND status='failed'",
            [id],
        )
        .map_err(|e| e.to_string())
    });
    if let Err(error) = result {
        return page_error(&error, StatusCode::INTERNAL_SERVER_ERROR);
    }
    let _ = process_due(&state).await;
    Redirect::to("/?retried=1").into_response()
}
async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some((token, csrf)) = session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    let values = match form(body) {
        Ok(values) => values,
        Err(error) => return page_error(&error, StatusCode::BAD_REQUEST),
    };
    if !csrf_valid(&values, &csrf) {
        return page_error("表单验证失败，请刷新页面", StatusCode::BAD_REQUEST);
    }
    if let Ok(con) = connection(&state) {
        let _ = con.execute("DELETE FROM sessions WHERE token_hash=?1", [hash(&token)]);
    }
    let mut response = Redirect::to("/login").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    response
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let password = env::var("PUBLISHER_PASSWORD").unwrap_or_default();
    if password.is_empty() {
        return Err("请先设置 PUBLISHER_PASSWORD".into());
    }
    let timezone: Tz = env::var("PUBLISHER_TIMEZONE")
        .unwrap_or_else(|_| "Asia/Seoul".into())
        .parse()?;
    let state = Arc::new(AppState {
        db_path: env::var("PUBLISHER_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("publisher.sqlite3")),
        password,
        telegram_token: env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
        slack_token: env::var("SLACK_BOT_TOKEN").unwrap_or_default(),
        timezone,
        targets: parse_targets(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?,
        secure_cookie: env::var("PUBLISHER_HTTPS").as_deref() == Ok("1"),
    });
    init_db(&state)?;
    let scheduler_state = state.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = process_due(&scheduler_state).await {
                eprintln!("定时任务错误: {error}");
            }
            sleep(Duration::from_secs(5)).await;
        }
    });
    let host = env::var("PUBLISHER_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = env::var("PUBLISHER_PORT").unwrap_or_else(|_| "8787".into());
    let address = format!("{host}:{port}");
    let app = Router::new()
        .route("/", get(dashboard))
        .route("/login", get(login_page).post(login))
        .route("/preview", post(preview))
        .route("/create", post(create))
        .route("/retry", post(retry))
        .route("/logout", post(logout))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("频道发布台: http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(db_path: PathBuf) -> AppState {
        let mut targets = HashMap::new();
        targets.insert(
            "telegram:@test_channel".into(),
            Target {
                platform: "telegram".into(),
                id: "@test_channel".into(),
                label: "Test".into(),
            },
        );
        AppState {
            db_path,
            password: "test-password".into(),
            telegram_token: String::new(),
            slack_token: String::new(),
            timezone: chrono_tz::Asia::Seoul,
            targets,
            client: reqwest::Client::new(),
            secure_cookie: false,
        }
    }

    #[test]
    fn html_escaping_handles_untrusted_content() {
        assert_eq!(escape("<script>\"&"), "&lt;script&gt;&quot;&amp;");
    }
    #[test]
    fn constant_compare_accepts_equal_passwords() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
    }

    #[test]
    fn validates_target_and_url_before_saving() {
        let state =
            test_state(std::env::temp_dir().join(format!("publisher-{}.sqlite3", random_token())));
        let mut values = HashMap::new();
        values.insert("body".into(), vec!["hello".into()]);
        values.insert("target".into(), vec!["telegram:@test_channel".into()]);
        assert!(validate_post(&state, &values).is_ok());
        values.insert("link".into(), vec!["file:///private/data".into()]);
        assert!(validate_post(&state, &values).is_err());
        values.remove("link");
        values.insert("target".into(), vec!["telegram:@other".into()]);
        assert!(validate_post(&state, &values).is_err());
    }

    #[tokio::test]
    async fn due_delivery_fails_without_token_and_is_recorded() {
        let path = std::env::temp_dir().join(format!("publisher-{}.sqlite3", random_token()));
        let state = test_state(path.clone());
        init_db(&state).unwrap();
        {
            let con = connection(&state).unwrap();
            con.execute("INSERT INTO posts(title,body,link,image_url,due_at,created_at) VALUES ('test','hello','','',?1,?1)", [now() - 1]).unwrap();
            con.execute("INSERT INTO deliveries(post_id,target_key,status) VALUES (1,'telegram:@test_channel','queued')", []).unwrap();
        }
        process_due(&state).await.unwrap();
        {
            let con = connection(&state).unwrap();
            let (status, attempts, error): (String, i64, String) = con
                .query_row(
                    "SELECT status, attempts, error FROM deliveries WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(status, "failed");
            assert_eq!(attempts, 1);
            assert!(error.contains("TELEGRAM_BOT_TOKEN"));
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
