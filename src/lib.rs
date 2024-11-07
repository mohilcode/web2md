use worker::*;
use worker_macros::event;
use serde::Deserialize;
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::cell::RefCell;

#[derive(Debug, Deserialize)]
struct ConvertRequest {
    url: String,
    #[serde(default)]
    config: ConvertConfig,
}

#[derive(Debug, Deserialize, Default)]
struct ConvertConfig {
    include_links: bool,
    clean_whitespace: bool,
    cleaning_rules: CleaningRules,
    preserve_headings: bool,
    include_metadata: bool,
    max_heading_level: u8,
}

#[derive(Debug, Deserialize, Default)]
struct CleaningRules {
    remove_scripts: bool,
    remove_styles: bool,
    remove_comments: bool,
    preserve_line_breaks: bool,
}

struct MetadataHandler {
    title: Option<String>,
    author: Option<String>,
    date: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
}

impl MetadataHandler {
    fn new() -> Self {
        Self {
            title: None,
            author: None,
            date: None,
            description: None,
            tags: Vec::new(),
        }
    }

    fn format_metadata(&self) -> String {
        let mut metadata = String::new();

        if let Some(title) = &self.title {
            metadata.push_str(&format!("# {}\n\n", title));
        }

        metadata.push_str("---\n");

        if let Some(author) = &self.author {
            metadata.push_str(&format!("Author: {}\n", author));
        }
        if let Some(date) = &self.date {
            metadata.push_str(&format!("Date: {}\n", date));
        }
        if let Some(description) = &self.description {
            metadata.push_str(&format!("Description: {}\n", description));
        }
        if !self.tags.is_empty() {
            metadata.push_str(&format!("Tags: {}\n", self.tags.join(", ")));
        }

        metadata.push_str("---\n\n");
        metadata
    }
}

struct MarkdownFormatter {
    config: ConvertConfig,
    content: String,
    indent_level: usize,
    list_type_stack: Vec<ListType>,
    in_table: bool,
    table_columns: Vec<String>,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    metadata: MetadataHandler,
}

#[derive(Clone, Copy)]
enum ListType {
    Ordered(u8),
    Unordered,
}

lazy_static! {
    static ref INLINE_TAGS: HashMap<&'static str, (&'static str, &'static str)> = {
        let mut m = HashMap::new();
        m.insert("strong", ("**", "**"));
        m.insert("b", ("**", "**"));
        m.insert("em", ("*", "*"));
        m.insert("i", ("*", "*"));
        m.insert("code", ("`", "`"));
        m.insert("mark", ("==", "=="));
        m.insert("del", ("~~", "~~"));
        m.insert("ins", ("__", "__"));
        m
    };

    static ref BLOCK_TAGS: HashMap<&'static str, BlockType> = {
        let mut m = HashMap::new();
        m.insert("p", BlockType::Paragraph);
        m.insert("div", BlockType::Div);
        m.insert("article", BlockType::Article);
        m.insert("section", BlockType::Section);
        m.insert("table", BlockType::Table);
        m.insert("tr", BlockType::TableRow);
        m.insert("td", BlockType::TableCell);
        m.insert("th", BlockType::TableHeader);
        m
    };

    static ref WHITESPACE_REGEX: Regex = Regex::new(r"\s+").unwrap();
    static ref URL_REGEX: Regex = Regex::new(r"^https?://").unwrap();
}

#[derive(Copy, Clone)]
enum BlockType {
    Paragraph,
    Div,
    Article,
    Section,
    Table,
    TableRow,
    TableCell,
    TableHeader,
}

impl MarkdownFormatter {
    fn new(config: ConvertConfig) -> Self {
        Self {
            config,
            content: String::with_capacity(4096),
            indent_level: 0,
            list_type_stack: Vec::new(),
            in_table: false,
            table_columns: Vec::new(),
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            metadata: MetadataHandler::new(),
        }
    }

    fn should_skip_node(&self, handle: &Handle) -> bool {
        if !self.config.cleaning_rules.remove_scripts
           && !self.config.cleaning_rules.remove_styles
           && !self.config.cleaning_rules.remove_comments {
            return false;
        }

        match &handle.data {
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();
                (self.config.cleaning_rules.remove_scripts && tag == "script") ||
                (self.config.cleaning_rules.remove_styles && tag == "style")
            }
            NodeData::Comment { .. } => self.config.cleaning_rules.remove_comments,
            NodeData::ProcessingInstruction { .. } => true,
            _ => false
        }
    }

    fn clean_text(&self, text: &str) -> String {
        if !self.config.clean_whitespace {
            return text.to_string();
        }

        let cleaned = WHITESPACE_REGEX
            .replace_all(text.trim(), " ")
            .to_string();

        if cleaned.chars().all(char::is_whitespace) {
            String::new()
        } else {
            cleaned
        }
    }

    fn process_node(&mut self, handle: &Handle) {
        if self.should_skip_node(handle) {
            return;
        }

        match &handle.data {
            NodeData::Element { name, attrs, .. } => {
                let tag_name = name.local.as_ref();

                match tag_name {
                    name @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                        if self.config.preserve_headings {
                            let level = name[1..].parse::<usize>().unwrap();
                            if level as u8 <= self.config.max_heading_level {
                                self.process_header(handle, level);
                            }
                        }
                    }

                    "a" => self.process_link(handle, attrs),
                    "img" => self.process_image(handle, attrs),
                    "meta" if self.config.include_metadata => self.extract_metadata(handle, attrs),

                    "table" => {
                        self.in_table = true;
                        self.table_columns.clear();
                        self.table_rows.clear();
                        self.process_children(handle);
                        self.format_table();
                        self.in_table = false;
                    }

                    "tr" if self.in_table => {
                        self.current_row.clear();
                        self.process_children(handle);
                        if !self.current_row.is_empty() {
                            self.table_rows.push(self.current_row.clone());
                        }
                    }

                    "th" | "td" if self.in_table => {
                        self.current_cell.clear();
                        self.process_children(handle);
                        self.current_row.push(self.current_cell.trim().to_string());
                    }

                    "ul" => self.process_list(handle, ListType::Unordered),
                    "ol" => self.process_list(handle, ListType::Ordered(1)),

                    tag if INLINE_TAGS.contains_key(tag) => {
                        let (prefix, suffix) = INLINE_TAGS[tag];
                        self.content.push_str(prefix);
                        self.process_children(handle);
                        self.content.push_str(suffix);
                    }

                    tag if BLOCK_TAGS.contains_key(tag) => {
                        self.add_double_newline();
                        self.process_children(handle);
                        self.add_double_newline();
                    }

                    _ => {
                        self.process_children(handle);
                    }
                }
            }

            NodeData::Text { contents } => {
                let text = contents.borrow();
                let processed_text = self.clean_text(&text);

                if self.in_table {
                    self.current_cell.push_str(&processed_text);
                } else {
                    self.content.push_str(&processed_text);
                }
            }

            _ => self.process_children(handle),
        }
    }

    fn process_header(&mut self, handle: &Handle, level: usize) {
        self.add_double_newline();
        self.content.push_str(&"#".repeat(level));
        self.content.push(' ');
        self.process_children(handle);
        self.add_double_newline();
    }

    fn process_link(&mut self, handle: &Handle, attrs: &RefCell<Vec<html5ever::Attribute>>) {
        if !self.config.include_links {
            self.process_children(handle);
            return;
        }

        let href = attrs.borrow()
            .iter()
            .find(|attr| attr.name.local.as_ref() == "href")
            .map(|attr| attr.value.to_string());

        let old_content = self.content.clone();
        self.content.clear();

        self.process_children(handle);

        let text = self.content.trim().to_string();

        self.content = old_content;

        if let Some(url) = href {
            if !text.is_empty() && text != url {
                self.content.push_str(&format!("[{}]({})", text, url));
            } else {
                self.content.push_str(&format!("<{}>", url));
            }
        }
    }

    fn process_image(&mut self, _handle: &Handle, attrs: &RefCell<Vec<html5ever::Attribute>>) {
        let attrs = attrs.borrow();
        let src = attrs.iter()
            .find(|attr| attr.name.local.as_ref() == "src")
            .map(|attr| attr.value.to_string());

        let alt = attrs.iter()
            .find(|attr| attr.name.local.as_ref() == "alt")
            .map(|attr| attr.value.to_string())
            .unwrap_or_default();

        if let Some(url) = src {
            self.add_newline();
            self.content.push_str(&format!("![{}]({})", alt, url));
            self.add_newline();
        }
    }

    fn process_list(&mut self, handle: &Handle, list_type: ListType) {
        self.list_type_stack.push(list_type);
        self.indent_level += match list_type {
            ListType::Unordered => 2,
            ListType::Ordered(_) => 3,
        };

        let mut current_count = match list_type {
            ListType::Ordered(start) => start,
            _ => 1,
        };

        for child in handle.children.borrow().iter() {
            if let NodeData::Element { ref name, .. } = child.data {
                if name.local.as_ref() == "li" {
                    let prefix = match list_type {
                        ListType::Unordered => "* ".to_string(),
                        ListType::Ordered(_) => format!("{}. ", current_count),
                    };
                    self.content.push_str(&" ".repeat(self.indent_level));
                    self.content.push_str(&prefix);
                    self.process_node(child);
                    self.add_newline();
                    current_count += 1;
                }
            }
        }

        self.list_type_stack.pop();
        self.indent_level -= match list_type {
            ListType::Unordered => 2,
            ListType::Ordered(_) => 3,
        };
        self.add_newline();
    }

    fn extract_metadata(&mut self, _handle: &Handle, attrs: &RefCell<Vec<html5ever::Attribute>>) {
        let attrs = attrs.borrow();

        if let Some(property) = attrs.iter().find(|attr| attr.name.local.as_ref() == "property") {
            if let Some(content) = attrs.iter().find(|attr| attr.name.local.as_ref() == "content") {
                match property.value.as_ref() {
                    "og:title" => self.metadata.title = Some(content.value.to_string()),
                    "og:description" => self.metadata.description = Some(content.value.to_string()),
                    "article:author" => self.metadata.author = Some(content.value.to_string()),
                    "article:published_time" => self.metadata.date = Some(content.value.to_string()),
                    "article:tag" => self.metadata.tags.push(content.value.to_string()),
                    _ => {}
                }
            }
        }
    }

    fn process_children(&mut self, handle: &Handle) {
        for child in handle.children.borrow().iter() {
            self.process_node(child);
        }
    }

    fn add_newline(&mut self) {
        if !self.content.ends_with('\n') {
            self.content.push('\n');
        }
    }

    fn add_double_newline(&mut self) {
        self.add_newline();
        self.add_newline();
    }

    fn format_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        let col_count = self.table_rows[0].len();
        let mut col_widths = vec![0; col_count];

        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    col_widths[i] = col_widths[i].max(cell.len());
                }
            }
        }

        self.add_double_newline();

        let rows_to_process = self.table_rows.clone();

        if let Some(header_row) = rows_to_process.first() {
            self.format_table_row(header_row, &col_widths);

            self.content.push('|');
            for width in &col_widths {
                self.content.push_str(&format!(" {} |", "-".repeat(*width)));
            }
            self.add_newline();
        }

        for row in rows_to_process.iter().skip(1) {
            self.format_table_row(row, &col_widths);
        }

        self.add_newline();
    }

    fn format_table_row(&mut self, row: &[String], col_widths: &[usize]) {
        self.content.push('|');
        for (i, cell) in row.iter().enumerate() {
            if i < col_widths.len() {
                let padding = " ".repeat(col_widths[i] - cell.len());
                self.content.push_str(&format!(" {}{} |", cell, padding));
            }
        }
        self.add_newline();
    }

    fn result(self) -> String {
        let mut final_content = String::with_capacity(self.content.len() + 1000);

        if self.config.include_metadata {
            final_content.push_str(&self.metadata.format_metadata());
        }

        final_content.push_str(&self.content.trim());

        if self.config.clean_whitespace && !self.config.cleaning_rules.preserve_line_breaks {
            let cleaned = WHITESPACE_REGEX
                .replace_all(&final_content, "\n\n")
                .to_string();
            cleaned.trim().to_string()
        } else {
            final_content.trim().to_string()
        }
    }
}

fn html_to_markdown(html: &str, config: ConvertConfig) -> String {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    let mut formatter = MarkdownFormatter::new(config);
    formatter.process_node(&dom.document);
    formatter.result()
}

async fn fetch_url_with_timeout(url: &str, _timeout_ms: u32) -> Result<String> {
    let mut opts = RequestInit::new();
    opts.method = Method::Get;
    opts.headers = Headers::from_iter([
        ("User-Agent", "Mozilla/5.0 (compatible; Cloudflare Worker)"),
    ]);

    let request = Request::new_with_init(url, &opts)?;

    console_log!("Fetching URL: {}", url);

    let mut response = match Fetch::Request(request).send().await {
        Ok(resp) => resp,
        Err(e) => {
            console_error!("Fetch error: {:?}", e);
            return Err(Error::RustError(format!("Failed to fetch URL: {}", e)));
        }
    };

    if response.status_code() >= 400 {
        console_error!("HTTP error: {}", response.status_code());
        return Err(Error::RustError(format!("HTTP error: {}", response.status_code())));
    }

    match response.text().await {
        Ok(text) => Ok(text),
        Err(e) => {
            console_error!("Text extraction error: {:?}", e);
            Err(Error::RustError(format!("Failed to extract text: {}", e)))
        }
    }
}

async fn fetch_and_convert(req: ConvertRequest) -> Result<String> {
    let html = fetch_url_with_timeout(&req.url, 10000).await?;

    Ok(html_to_markdown(&html, req.config))
}

#[event(fetch)]
pub async fn main(mut req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    match req.method() {
        Method::Post => {
            let request: ConvertRequest = match req.json().await {
                Ok(req) => req,
                Err(e) => {
                    console_error!("JSON parsing error: {:?}", e);
                    return Response::error(
                        format!("Invalid request format: {}", e),
                        400
                    );
                }
            };

            console_log!("Processing URL: {}", request.url);

            match fetch_and_convert(request).await {
                Ok(markdown) => {
                    let headers = Headers::from_iter([
                        ("Access-Control-Allow-Origin", "*"),
                        ("Content-Type", "text/markdown; charset=utf-8"),
                        ("Cache-Control", "public, max-age=3600"),
                    ]);

                    Response::ok(markdown)
                        .map(|resp| resp.with_headers(headers))
                }
                Err(e) => {
                    console_error!("Conversion error: {:?}", e);
                    Response::error(format!("Conversion failed: {}", e), 500)
                }
            }
        }
        _ => Response::error("Method Not Allowed", 405)
    }
}
