use worker::*;
use worker_macros::event;
use serde::Deserialize;
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref INLINE_ELEMENTS: std::collections::HashSet<&'static str> = {
        let mut set = std::collections::HashSet::new();
        set.insert("a");
        set.insert("span");
        set.insert("strong");
        set.insert("em");
        set.insert("b");
        set.insert("i");
        set.insert("code");
        set
    };

    static ref BLOCK_ELEMENTS: std::collections::HashSet<&'static str> = {
        let mut set = std::collections::HashSet::new();
        set.insert("div");
        set.insert("p");
        set.insert("h1");
        set.insert("h2");
        set.insert("h3");
        set.insert("h4");
        set.insert("h5");
        set.insert("h6");
        set.insert("ul");
        set.insert("ol");
        set.insert("li");
        set.insert("pre");
        set
    };

    static ref WHITESPACE_REGEX: Regex = Regex::new(r"\s+").unwrap();
}

#[derive(Debug, Deserialize)]
struct ConvertRequest {
    url: String,
    #[serde(default)]
    include_links: bool,
    #[serde(default)]
    clean_whitespace: bool,
}

struct ConversionContext {
    include_links: bool,
    clean_whitespace: bool,
    markdown: String,
    last_was_block: bool,
    buffer: Vec<u8>,
}

impl ConversionContext {
    #[inline]
    fn new(include_links: bool, clean_whitespace: bool, initial_capacity: usize) -> Self {
        Self {
            include_links,
            clean_whitespace,
            markdown: String::with_capacity(initial_capacity),
            last_was_block: false,
            buffer: Vec::with_capacity(1024),
        }
    }

    #[inline]
    fn append(&mut self, s: &str) {
        if self.clean_whitespace {
            let cleaned = WHITESPACE_REGEX.replace_all(s.trim(), " ");
            self.markdown.push_str(&cleaned);
        } else {
            self.markdown.push_str(s);
        }
    }

    #[inline]
    fn append_char(&mut self, c: char) {
        self.markdown.push(c);
    }

    #[inline]
    fn append_newline(&mut self) {
        if !self.last_was_block {
            self.markdown.push('\n');
            self.last_was_block = true;
        }
    }

    #[inline]
    fn flush_buffer(&mut self) {
        if !self.buffer.is_empty() {
            if let Ok(s) = String::from_utf8(self.buffer.clone()) {
                self.append(&s);
            }
            self.buffer.clear();
        }
    }
}

#[inline]
fn process_node(handle: &Handle, ctx: &mut ConversionContext) {
    match &handle.data {
        NodeData::Element { name, attrs, .. } => {
            let tag_name = name.local.as_ref();

            if BLOCK_ELEMENTS.contains(tag_name) {
                ctx.flush_buffer();
                ctx.append_newline();
            }

            match tag_name {
                "p" => {
                    process_children(handle, ctx);
                    ctx.append_newline();
                },
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag_name.as_bytes()[1] - b'0';
                    ctx.append(&"#".repeat(level as usize));
                    ctx.append_char(' ');
                    process_children(handle, ctx);
                    ctx.append_newline();
                },
                "a" if ctx.include_links => {
                    if let Some(href) = attrs.borrow()
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "href")
                        .map(|attr| &attr.value)
                    {
                        ctx.append_char('[');
                        process_children(handle, ctx);
                        ctx.append("](");
                        ctx.append(href);
                        ctx.append_char(')');
                    } else {
                        process_children(handle, ctx);
                    }
                },
                "li" => {
                    ctx.append("* ");
                    process_children(handle, ctx);
                    ctx.append_newline();
                },
                _ => process_children(handle, ctx),
            }
        },
        NodeData::Text { contents } => {
            let text = contents.borrow();
            if !text.trim().is_empty() {
                ctx.buffer.extend_from_slice(text.as_bytes());
            }
        },
        _ => process_children(handle, ctx),
    }
}

#[inline]
fn process_children(handle: &Handle, ctx: &mut ConversionContext) {
    for child in handle.children.borrow().iter() {
        process_node(child, ctx);
    }
}

fn html_to_markdown(html: &str, include_links: bool, clean_whitespace: bool) -> String {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    let mut ctx = ConversionContext::new(
        include_links,
        clean_whitespace,
        html.len() / 2
    );

    process_node(&dom.document, &mut ctx);
    ctx.flush_buffer();
    ctx.markdown.trim().to_string()
}

async fn fetch_and_convert(url: String, include_links: bool, clean_whitespace: bool) -> Result<String> {
    let mut response = Fetch::Url(url.parse().unwrap())
        .send()
        .await?;

    let html = response.text().await?;
    Ok(html_to_markdown(&html, include_links, clean_whitespace))
}

#[event(fetch)]
pub async fn main(mut req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    match req.method() {
        Method::Options => {
            let mut headers = Headers::new();
            headers.append("Access-Control-Allow-Origin", "*")?;
            headers.append("Access-Control-Allow-Methods", "POST, OPTIONS")?;
            headers.append("Access-Control-Allow-Headers", "Content-Type")?;

            let response = Response::empty()?
                .with_headers(headers);
            Ok(response.with_status(204))
        },
        Method::Post => {
            let request: ConvertRequest = req.json().await
                .map_err(|_| Error::from("Invalid JSON body"))?;

            match fetch_and_convert(
                request.url,
                request.include_links,
                request.clean_whitespace
            ).await {
                Ok(markdown) => {
                    let mut headers = Headers::new();
                    headers.append("Access-Control-Allow-Origin", "*")?;
                    headers.append("Content-Type", "text/markdown; charset=utf-8")?;

                    let response = Response::ok(markdown)?
                        .with_headers(headers);
                    Ok(response)
                },
                Err(e) => Response::error(format!("Conversion failed: {}", e), 500),
            }
        },
        _ => Response::error("Method Not Allowed", 405),
    }
}