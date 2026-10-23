//! Drawing a [`TransferBoard`]: a summary line, then one block per transfer, newest first —
//! a status line, a chunk grid, and two detail lines — and a legend.
//!
//! Pure like the rest of the view: the frame is a function of the board alone, its throughput
//! read at the board's own clock. Each grid cell's glyph names its state, so a plain-text
//! snapshot shows the grid as a terminal does without its colours.

use capsule_sdk::events::RetryReason;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{Cell, ChunkState, Direction, Phase, RetryNote, Transfer, TransferBoard};

/// Rows of grid cells per transfer.
pub const GRID_ROWS: u16 = 2;

/// Lines one transfer takes: status, grid, detail.
const TRANSFER_LINES: u16 = 1 + GRID_ROWS + 2;

/// Lines the legend takes.
const LEGEND_LINES: u16 = 2;

/// A content address shortened to this many characters.
const BLOB_CHARS: usize = 12;

/// Every cell state with its description, in the legend's two lines: progress, then trouble.
const LEGEND: [&[(Cell, &str)]; 2] = [
    &[
        (Cell::Pending, "pending"),
        (Cell::Held, "held before this run"),
        (Cell::InFlight, "in flight"),
        (Cell::Acked, "acked"),
        (Cell::Verified, "verified"),
    ],
    &[
        (Cell::Retrying, "retrying"),
        (Cell::Rejected, "rejected"),
        (Cell::AckedAfterRetry, "acked after a retry"),
        (Cell::AckedAfterReject, "acked after a reject"),
    ],
];

/// The glyph and colour a cell draws with.
#[must_use]
pub fn glyph(cell: Cell) -> (&'static str, Style) {
    let (symbol, color) = match cell {
        Cell::Pending => ("·", Color::DarkGray),
        Cell::Held => ("=", Color::Blue),
        Cell::InFlight => ("░", Color::Cyan),
        Cell::Acked => ("▓", Color::Green),
        Cell::Verified => ("█", Color::Green),
        Cell::Retrying => ("R", Color::Yellow),
        Cell::Rejected => ("X", Color::Red),
        Cell::AckedAfterRetry => ("r", Color::Yellow),
        Cell::AckedAfterReject => ("x", Color::Red),
    };
    let style = Style::new().fg(color);
    let style = if cell == Cell::Verified {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    };
    (symbol, style)
}

/// Draw `board` into `area` as the transfer screen's body, under a block titled `title`.
pub fn render(frame: &mut Frame, area: Rect, board: &TransferBoard, title: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_owned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines(board, inner)), inner);
}

/// The body's lines for an inner area of `area`'s size.
fn lines(board: &TransferBoard, area: Rect) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let mut lines = vec![summary(board)];
    if board.transfers().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("No transfers yet."));
        lines.push(Line::from(
            "Uploads, pushes and fetches started in this session appear here as they run.",
        ));
        return lines;
    }
    // One line for the summary and the legend's lines; the rest holds whole transfers.
    let room = area.height.saturating_sub(1 + LEGEND_LINES) / TRANSFER_LINES;
    let shown = usize::from(room).min(board.transfers().len());
    let hidden = board.transfers().len() - shown;
    for transfer in board.transfers().iter().rev().take(shown) {
        lines.push(status(transfer, board.now_ms()));
        lines.extend(grid(transfer, width));
        lines.extend(detail(transfer));
    }
    if hidden > 0 {
        lines.push(Line::from(
            format!("… and {hidden} older").fg(Color::DarkGray),
        ));
    } else {
        lines.extend(legend());
    }
    lines
}

fn summary(board: &TransferBoard) -> Line<'static> {
    let custody = board.custody();
    let mut spans = Vec::new();
    // First, so a narrow terminal never cuts it off: the totals below are short by this much.
    if board.missed() > 0 {
        spans.push(format!("missed {} events · ", board.missed()).fg(Color::Red));
    }
    let count = |n: usize, one, many| {
        format!(
            "{n} {}",
            plural(u32::try_from(n).unwrap_or(u32::MAX), one, many)
        )
    };
    spans.push(Span::from(format!(
        "{}, {} active · {}/s · custody: {} durable, {} not, {}",
        count(board.transfers().len(), "transfer", "transfers"),
        board.active(),
        bytes(board.throughput()),
        custody.durable,
        custody.not_durable,
        count(
            usize::try_from(custody.receipts).unwrap_or(usize::MAX),
            "receipt",
            "receipts"
        ),
    )));
    Line::from(spans)
}

fn status(transfer: &Transfer, now_ms: u64) -> Line<'static> {
    let arrow = match transfer.direction {
        Direction::Upload => "↑",
        Direction::Fetch => "↓",
    };
    let blob: String = transfer.blob.chars().take(BLOB_CHARS).collect();
    let progress = match transfer.size {
        Some(size) if size > 0 => format!(
            "{} / {} {:>3}%",
            bytes(transfer.done),
            bytes(size),
            u128::from(transfer.done.min(size)) * 100 / u128::from(size)
        ),
        Some(_) => "0 B".to_owned(),
        None => format!("{} / ?", bytes(transfer.done)),
    };
    Line::from(vec![
        Span::from(format!("{arrow} ")).bold(),
        Span::from(blob).bold(),
        Span::from(format!(
            "  {progress}  {}/s  ",
            bytes(transfer.throughput(now_ms))
        )),
        phase(transfer),
    ])
}

fn phase(transfer: &Transfer) -> Span<'static> {
    match transfer.phase {
        Phase::Active => match transfer.active.map(|a| a.state) {
            Some(ChunkState::InFlight) => match transfer.direction {
                Direction::Upload => "sending".fg(Color::Cyan),
                Direction::Fetch => "fetching".fg(Color::Cyan),
            },
            Some(ChunkState::Retrying) => "retrying".fg(Color::Yellow),
            Some(ChunkState::Rejected) => "re-sending rejected chunk".fg(Color::Red),
            None if transfer.done == 0 && transfer.sessions == 0 => "starting".fg(Color::DarkGray),
            None => "waiting".fg(Color::DarkGray),
        },
        Phase::Finalizing => "sent; finalizing".fg(Color::Green),
        Phase::Verified => match transfer.receipt_seq {
            Some(seq) => format!("verified (receipt {seq})").fg(Color::Green).bold(),
            None => "verified".fg(Color::Green).bold(),
        },
        Phase::Merged => "already on the server".fg(Color::Green),
        Phase::IntegrityRejected => "hash mismatch; discarded".fg(Color::Red),
    }
}

/// The grid as [`GRID_ROWS`] lines of `width` cells, or one explanatory line padded to the same
/// height while the size is unknown.
fn grid(transfer: &Transfer, width: usize) -> Vec<Line<'static>> {
    let rows = usize::from(GRID_ROWS);
    let Some(cells) = transfer.cells(width.max(1) * rows) else {
        let note = match transfer.phase {
            Phase::Merged => "nothing to send",
            _ => "size not known yet",
        };
        let mut lines = vec![Line::from(note.fg(Color::DarkGray))];
        lines.resize(rows, Line::default());
        return lines;
    };
    let mut lines: Vec<Line<'static>> = cells
        .chunks(width.max(1))
        .map(|row| {
            Line::from(
                row.iter()
                    .map(|cell| {
                        let (symbol, style) = glyph(*cell);
                        Span::styled(symbol, style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    lines.resize(rows, Line::default());
    lines
}

/// Two lines: the counters (chunk size and its changes, retries, rejects, session), then what
/// happened last (the last retry's reason, where it resumed). The second is blank when nothing
/// has, which also separates one transfer from the next.
fn detail(transfer: &Transfer) -> [Line<'static>; 2] {
    let mut counters = Vec::new();
    counters.push(match transfer.chunk_size() {
        None => "chunk —".to_owned(),
        Some(size) if transfer.chunk_size_changes == 0 => format!("chunk {}", bytes(size)),
        Some(_) => format!(
            "chunk {} ({} {})",
            transfer
                .chunk_sizes
                .iter()
                .map(|s| bytes(*s))
                .collect::<Vec<_>>()
                .join(" → "),
            transfer.chunk_size_changes,
            plural(transfer.chunk_size_changes, "change", "changes"),
        ),
    });
    counters.push(format!(
        "{} {}",
        transfer.retries,
        plural(transfer.retries, "retry", "retries")
    ));
    counters.push(format!(
        "{} {}",
        transfer.rejects,
        plural(transfer.rejects, "reject", "rejects")
    ));
    if let Some(session) = &transfer.session_id {
        counters.push(if transfer.sessions > 1 {
            format!("{session} ({} sessions)", transfer.sessions)
        } else {
            session.clone()
        });
    }

    let mut history = Vec::new();
    if let Some(note) = transfer.last_retry {
        history.push(format!("last retry: {}", retry(note)));
    }
    if let Some(offset) = transfer.resumed_at {
        history.push(format!("resumed at {}", bytes(offset)));
    }
    [
        Line::from(counters.join(" · ").fg(Color::DarkGray)),
        Line::from(history.join(" · ").fg(Color::DarkGray)),
    ]
}

fn retry(note: RetryNote) -> String {
    match note {
        RetryNote::Upload(RetryReason::TransportReset { wait_ms }) => {
            format!("connection reset, waited {wait_ms} ms")
        }
        RetryNote::Upload(RetryReason::OffsetRealign { to }) => {
            format!("server offset realigned to {}", bytes(to))
        }
        RetryNote::Upload(RetryReason::SessionLost) => "session lost, opened a new one".to_owned(),
        RetryNote::Upload(RetryReason::ServerWait { wait_ms }) => {
            format!("server asked to wait {wait_ms} ms")
        }
        RetryNote::Fetch {
            stalled: true,
            wait_ms,
        } => format!("stalled, waited {wait_ms} ms"),
        RetryNote::Fetch {
            stalled: false,
            wait_ms,
        } => format!("no bytes, waited {wait_ms} ms"),
    }
}

fn legend() -> Vec<Line<'static>> {
    LEGEND
        .iter()
        .map(|entries| {
            let mut spans = Vec::new();
            for (cell, description) in *entries {
                if !spans.is_empty() {
                    spans.push(Span::from("  "));
                }
                let (symbol, style) = glyph(*cell);
                spans.push(Span::styled(symbol, style));
                spans.push(format!(" {description}").fg(Color::DarkGray));
            }
            Line::from(spans)
        })
        .collect()
}

fn plural(count: u32, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn bytes(count: u64) -> String {
    humansize::format_size(count, humansize::BINARY)
}

#[cfg(test)]
mod tests;
