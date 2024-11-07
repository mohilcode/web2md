use worker::*;
use worker_macros::event;
use serde::{Deserialize, Serialize};
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

const NEWLINE: &str = "\n";
const DOUBLE_NEWLINE: &str = "\n\n";
const CODE_BLOCK: &str = "\n```\n";
const LIST_ITEM: &str = "* ";

#[derive(Debug, Deserialize)]
struct ConvertRequest {
    url: String,
    #[serde(default)]
    include_links: bool,
}

#[derive(Debug, Serialize)]
struct ConvertResponse {
    markdown: String,
    stats: ConversionStats,
}

#[derive(Debug, Serialize)]
struct ConversionStats {
    original_size: usize,
    converted_size: usize,
}

#[derive(Debug)]

struct ConversionContext {
    include_links: bool,
    markdown: String,
}

impl ConversionContext {
    fn new(include_links: bool, initial_capacity: usize) -> Self {
        Self {
            include_links,
            markdown: String::with_capacity(initial_capacity),
        }
    }

    #[inline]
    fn append(&mut self, s: &str) {
        self.markdown.push_str(s);
    }

    #[inline]
    fn append_char(&mut self, c: char) {
        self.markdown.push(c);
    }
}

fn html_to_markdown(html: &str, include_links: bool) -> String {
    let estimated_capacity = html.len() / 2;

    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    let mut ctx = ConversionContext::new(include_links, estimated_capacity);
    process_node(&dom.document, &mut ctx);
    ctx.markdown.trim().to_string()
}

fn process_node(handle: &Handle, ctx: &mut ConversionContext) {
    let node = &handle.data;

    match node {
        NodeData::Element { ref name, ref attrs, .. } => {
            let tag_name = name.local.as_ref();

            match tag_name {
                "p" => {
                    ctx.append(DOUBLE_NEWLINE);
                    process_children(handle, ctx);
                    ctx.append(NEWLINE);
                },
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag_name.as_bytes()[1] - b'0';
                    ctx.append(NEWLINE);
                    ctx.append(&"#".repeat(level as usize));
                    ctx.append_char(' ');
                    process_children(handle, ctx);
                    ctx.append(NEWLINE);
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
                "strong" | "b" => {
                    ctx.append("**");
                    process_children(handle, ctx);
                    ctx.append("**");
                },
                "em" | "i" => {
                    ctx.append_char('*');
                    process_children(handle, ctx);
                    ctx.append_char('*');
                },
                "code" => {
                    ctx.append_char('`');
                    process_children(handle, ctx);
                    ctx.append_char('`');
                },
                "pre" => {
                    ctx.append(CODE_BLOCK);
                    process_children(handle, ctx);
                    ctx.append(CODE_BLOCK);
                },
                "ul" | "ol" => {
                    ctx.append(NEWLINE);
                    process_children(handle, ctx);
                    ctx.append(NEWLINE);
                },
                "li" => {
                    ctx.append(LIST_ITEM);
                    process_children(handle, ctx);
                    ctx.append(NEWLINE);
                },
                _ => process_children(handle, ctx),
            }
        },
        NodeData::Text { ref contents } => {
            let text = contents.borrow();
            if !text.trim().is_empty() {
                ctx.append(text.trim());
            }
        },
        _ => process_children(handle, ctx),
    }
}

fn process_children(handle: &Handle, ctx: &mut ConversionContext) {
    for child in handle.children.borrow().iter() {
        process_node(child, ctx);
    }
}

async fn fetch_and_convert(url: String, include_links: bool) -> Result<ConvertResponse> {
    let mut response = Fetch::Url(url.parse().unwrap())
        .send()
        .await?;

    let html = response.text().await?;
    let original_size = html.len();

    let markdown = html_to_markdown(&html, include_links);
    let converted_size = markdown.len();

    Ok(ConvertResponse {
        markdown,
        stats: ConversionStats {
            original_size,
            converted_size,
        },
    })
}

#[event(fetch)]
pub async fn main(mut req: Request, _env: Env, _ctx: Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    if req.method() == Method::Options {
        let mut response = Response::empty()?;
        response = response.with_cors(&Cors::default())?;
        return Ok(response.with_status(204));
    }

    if req.method() != Method::Post {
        return Response::error("Method Not Allowed", 405);
    }

    let request: ConvertRequest = req.json().await.map_err(|_| {
        Error::from("Invalid JSON body")
    })?;

    match fetch_and_convert(request.url, request.include_links).await {
        Ok(response_data) => {
            let mut response = Response::from_json(&response_data)?;
            response = response.with_cors(&Cors::default())?;
            Ok(response.with_status(200))
        },
        Err(_) => Response::error("Conversion failed", 500),
    }
}