/*
 * greed.rs - Rust port of Matthew T. Day's Greed.
 *
 * SPDX-FileCopyrightText: (C) Eric S. Raymond <esr@thyrsus.com>
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::env;
use std::fs::{remove_file, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::{align_of, size_of};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ME: char = '@';
const MAXSCORES: usize = 10;
const LOCKPATH: &str = "/tmp/Greed.lock";
const LOCALSCOREFILE: &str = ".greedscores";
const USERNAMELEN: usize = 32;
const USER_FIELD_LEN: usize = USERNAMELEN + 1;
const DEFAULT_RELEASE: &str = " (dev-build)";
const DEFAULT_SCOREFILE: &str = "/usr/games/lib/greed.hs";

type TimeT = i64;

#[derive(Clone, Copy, Default)]
struct CellAttr {
    color: Option<Color>,
    bold: bool,
}

#[derive(Clone, Copy)]
enum Color {
    Blue,
    Green,
    Cyan,
    Red,
    Magenta,
    Yellow,
    White,
}

#[derive(Clone)]
struct Config {
    height: i32,
    width: i32,
    maxstep: i32,
    status_row: i32,
    allmoves: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            height: 22,
            width: 79,
            maxstep: 9,
            status_row: 23,
            allmoves: false,
        }
    }
}

struct Game {
    config: Config,
    grid: Vec<i32>,
    y: i32,
    x: i32,
    score: i32,
    havebotmsg: bool,
}

#[derive(Clone, Copy)]
struct Score {
    user: [u8; USER_FIELD_LEN],
    time: TimeT,
    height: i32,
    width: i32,
    maxstep: i32,
    score: i32,
}

impl Default for Score {
    fn default() -> Self {
        Self {
            user: [0; USER_FIELD_LEN],
            time: 0,
            height: 0,
            width: 0,
            maxstep: 0,
            score: 0,
        }
    }
}

struct ScoreLayout {
    size: usize,
    time_offset: usize,
    height_offset: usize,
    width_offset: usize,
    maxstep_offset: usize,
    score_offset: usize,
}

struct UserInfo {
    name: String,
    home: PathBuf,
}

struct Lrand48 {
    state: u64,
}

impl Lrand48 {
    const MASK: u64 = (1u64 << 48) - 1;

    fn new(seed: i64) -> Self {
        Self {
            state: (((seed as u64) << 16) | 0x330e) & Self::MASK,
        }
    }

    fn next(&mut self) -> i32 {
        self.state = self
            .state
            .wrapping_mul(0x5deece66d)
            .wrapping_add(0xb)
            & Self::MASK;
        (self.state >> 17) as i32
    }

    fn rnd(&mut self, max: i32) -> i32 {
        (self.next() % max) + 1
    }
}

struct Terminal {
    saved_stty: Option<String>,
    stdout: io::Stdout,
}

impl Terminal {
    fn enter() -> io::Result<Self> {
        let saved_stty = stty_state();
        if saved_stty.is_some() {
            let _ = Command::new("stty")
                .args(["raw", "-echo"])
                .stdin(Stdio::inherit())
                .status();
        }

        let mut terminal = Self {
            saved_stty,
            stdout: io::stdout(),
        };
        terminal.write("\x1b[?25l")?;
        terminal.flush()?;
        Ok(terminal)
    }

    fn write(&mut self, text: &str) -> io::Result<()> {
        self.stdout.write_all(text.as_bytes())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        self.write("\x1b[2J")
    }

    fn move_to(&mut self, row: i32, col: i32) -> io::Result<()> {
        write!(self.stdout, "\x1b[{};{}H", row + 1, col + 1)
    }

    fn clear_to_eol(&mut self) -> io::Result<()> {
        self.write("\x1b[K")
    }

    fn set_attr(&mut self, attr: CellAttr, reverse: bool) -> io::Result<()> {
        self.write("\x1b[0")?;
        if attr.bold {
            self.write(";1")?;
        }
        if reverse {
            self.write(";7")?;
        }
        if let Some(color) = attr.color {
            write!(self.stdout, ";{}", color.sgr_code())?;
        }
        self.write("m")
    }

    fn reset_attr(&mut self) -> io::Result<()> {
        self.write("\x1b[0m")
    }

    fn addstr(&mut self, row: i32, col: i32, text: &str) -> io::Result<()> {
        self.move_to(row, col)?;
        self.write(text)
    }

    fn addch(&mut self, row: i32, col: i32, ch: char) -> io::Result<()> {
        self.move_to(row, col)?;
        write!(self.stdout, "{}", ch)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.reset_attr();
        let _ = self.write("\x1b[?25h");
        let _ = self.flush();
        if let Some(saved) = &self.saved_stty {
            let _ = Command::new("stty")
                .arg(saved)
                .stdin(Stdio::inherit())
                .status();
        }
    }
}

enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    Unknown,
}

enum MoveResult {
    Continue,
    Dead,
    Quit,
}

impl Color {
    fn sgr_code(self) -> i32 {
        match self {
            Color::Blue => 34,
            Color::Green => 32,
            Color::Cyan => 36,
            Color::Red => 31,
            Color::Magenta => 35,
            Color::Yellow => 33,
            Color::White => 37,
        }
    }
}

impl Game {
    fn new(config: Config, rng: &mut Lrand48) -> io::Result<Self> {
        let len = (config.height * config.width) as usize;
        let mut grid = vec![0; len];
        for row in 0..config.height {
            for col in 0..config.width {
                grid[(row * config.width + col) as usize] = rng.rnd(config.maxstep);
            }
        }

        let y = rng.rnd(config.height) - 1;
        let x = rng.rnd(config.width) - 1;
        grid[(y * config.width + x) as usize] = 0;

        Ok(Self {
            config,
            grid,
            y,
            x,
            score: 0,
            havebotmsg: false,
        })
    }

    fn get(&self, row: i32, col: i32) -> i32 {
        self.grid[(row * self.config.width + col) as usize]
    }

    fn set(&mut self, row: i32, col: i32, value: i32) {
        self.grid[(row * self.config.width + col) as usize] = value;
    }

    fn valid_path(&self, dy: i32, dx: i32) -> Option<i32> {
        if self.y + dy < 0
            || self.x + dx < 0
            || self.y + dy >= self.config.height
            || self.x + dx >= self.config.width
        {
            return None;
        }

        let distance = self.get(self.y + dy, self.x + dx);
        if distance == 0 {
            return None;
        }

        let mut row = self.y;
        let mut col = self.x;
        for _ in 0..distance {
            row += dy;
            col += dx;
            if row < 0
                || col < 0
                || row >= self.config.height
                || col >= self.config.width
                || self.get(row, col) == 0
            {
                return None;
            }
        }

        Some(distance)
    }

    fn othermove(&self, bady: i32, badx: i32) -> bool {
        for dy in -1..=1 {
            for dx in -1..=1 {
                if (dy == 0 && dx == 0) || (dy == bady && dx == badx) {
                    continue;
                }
                if self.valid_path(dy, dx).is_some() {
                    return true;
                }
            }
        }
        false
    }

    fn handle_key(
        &mut self,
        key: Key,
        term: &mut Terminal,
        attribs: &[CellAttr; 10],
    ) -> io::Result<MoveResult> {
        let (dy, dx) = match key {
            Key::Char('h') | Key::Char('H') | Key::Char('4') | Key::Left => (0, -1),
            Key::Char('j') | Key::Char('J') | Key::Char('2') | Key::Down => (1, 0),
            Key::Char('k') | Key::Char('K') | Key::Char('8') | Key::Up => (-1, 0),
            Key::Char('l') | Key::Char('L') | Key::Char('6') | Key::Right => (0, 1),
            Key::Char('b') | Key::Char('B') | Key::Char('1') => (1, -1),
            Key::Char('n') | Key::Char('N') | Key::Char('3') => (1, 1),
            Key::Char('y') | Key::Char('Y') | Key::Char('7') => (-1, -1),
            Key::Char('u') | Key::Char('U') | Key::Char('9') => (-1, 1),
            Key::Char('p') | Key::Char('P') => {
                self.config.allmoves = !self.config.allmoves;
                self.render(term, attribs)?;
                return Ok(MoveResult::Continue);
            }
            Key::Char('q') | Key::Char('Q') | Key::Ctrl('c') | Key::Ctrl('\\') => {
                if self.confirm_quit(term)? {
                    return Ok(MoveResult::Quit);
                }
                self.render(term, attribs)?;
                return Ok(MoveResult::Continue);
            }
            Key::Char('?') => {
                self.help(term, attribs)?;
                return Ok(MoveResult::Continue);
            }
            Key::Ctrl('l') | Key::Ctrl('r') => {
                self.render(term, attribs)?;
                return Ok(MoveResult::Continue);
            }
            _ => {
                self.render(term, attribs)?;
                return Ok(MoveResult::Continue);
            }
        };

        let Some(distance) = self.valid_path(dy, dx) else {
            if self.othermove(dy, dx) {
                self.botmsg(term, "Bad move.", true)?;
                return Ok(MoveResult::Continue);
            }

            self.die(dy, dx);
            self.render(term, attribs)?;
            return Ok(MoveResult::Dead);
        };

        self.move_by(dy, dx, distance);
        self.havebotmsg = false;
        self.render(term, attribs)?;
        Ok(MoveResult::Continue)
    }

    fn move_by(&mut self, dy: i32, dx: i32, distance: i32) {
        for _ in 0..distance {
            self.y += dy;
            self.x += dx;
            self.score += 1;
            self.set(self.y, self.x, 0);
        }
    }

    fn die(&mut self, dy: i32, dx: i32) {
        let mut row = self.y;
        let mut col = self.x;
        loop {
            let next_row = row + dy;
            let next_col = col + dx;
            if next_row < 0
                || next_col < 0
                || next_row >= self.config.height
                || next_col >= self.config.width
                || self.get(next_row, next_col) == 0
            {
                break;
            }
            row = next_row;
            col = next_col;
        }

        while self.y != row || self.x != col {
            self.y += dy;
            self.x += dx;
            self.score += 1;
        }
    }

    fn botmsg(&mut self, term: &mut Terminal, msg: &str, backcur: bool) -> io::Result<()> {
        term.addstr(self.config.status_row, 40, msg)?;
        term.clear_to_eol()?;
        if backcur {
            term.move_to(self.y, self.x)?;
        }
        term.flush()?;
        self.havebotmsg = true;
        Ok(())
    }

    fn confirm_quit(&mut self, term: &mut Terminal) -> io::Result<bool> {
        self.botmsg(term, "Really quit? ", false)?;
        let answer = read_key()?;
        Ok(matches!(answer, Key::Char('y') | Key::Char('Y')))
    }

    fn render(&self, term: &mut Terminal, attribs: &[CellAttr; 10]) -> io::Result<()> {
        for row in 0..self.config.height {
            for col in 0..self.config.width {
                if row == self.y && col == self.x {
                    term.set_attr(attribs[0], false)?;
                    term.addch(row, col, ME)?;
                    term.reset_attr()?;
                } else {
                    let value = self.get(row, col);
                    if value == 0 {
                        term.addch(row, col, ' ')?;
                    } else {
                        self.draw_digit(term, attribs, row, col, value, false)?;
                    }
                }
            }
        }

        if self.config.allmoves {
            self.showmoves(term, attribs)?;
            term.set_attr(attribs[0], false)?;
            term.addch(self.y, self.x, ME)?;
            term.reset_attr()?;
        }

        term.addstr(self.config.status_row, 0, "Score: ")?;
        term.addstr(
            self.config.status_row,
            7,
            &format!("{}  {:.2}%", self.score, score_perc(self.score, self.config.height, self.config.width)),
        )?;
        term.clear_to_eol()?;
        if self.havebotmsg {
            term.move_to(self.y, self.x)?;
        } else {
            term.addstr(
                self.config.status_row,
                40,
                &format!("{} - Hit '?' for help.", version()),
            )?;
            term.move_to(self.y, self.x)?;
        }
        term.flush()
    }

    fn draw_digit(
        &self,
        term: &mut Terminal,
        attribs: &[CellAttr; 10],
        row: i32,
        col: i32,
        value: i32,
        reverse: bool,
    ) -> io::Result<()> {
        term.set_attr(attribs[value as usize], reverse)?;
        term.addch(row, col, char::from(b'0' + value as u8))?;
        term.reset_attr()
    }

    fn showmoves(&self, term: &mut Terminal, attribs: &[CellAttr; 10]) -> io::Result<()> {
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dy == 0 && dx == 0 {
                    continue;
                }
                if let Some(distance) = self.valid_path(dy, dx) {
                    let mut row = self.y;
                    let mut col = self.x;
                    for _ in 0..distance {
                        row += dy;
                        col += dx;
                        self.draw_digit(term, attribs, row, col, self.get(row, col), true)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn help(&self, term: &mut Terminal, attribs: &[CellAttr; 10]) -> io::Result<()> {
        draw_box(term, 1, 7, 18, 65)?;
        term.addstr(
            2,
            9,
            &format!(
                "Welcome to {}, by Matthew Day <mday@iconsys.uu.net>.",
                version()
            ),
        )?;
        term.addstr(4, 9, " The object of Greed is to erase as much of the screen as")?;
        term.addstr(5, 9, " possible by moving around in a grid of numbers.  To move,")?;
        term.addstr(6, 9, " use the arrow keys, your number pad, or one of the letters")?;
        term.addstr(
            7,
            9,
            &format!(" 'hjklyubn'. Your location is signified by the '{}' symbol.", ME),
        )?;
        term.addstr(8, 9, " When you move in a direction, you erase N number of grid")?;
        term.addstr(9, 9, " squares in that direction, N being the first number in that")?;
        term.addstr(10, 9, " direction.  Your score reflects the total number of squares")?;
        term.addstr(11, 9, " eaten.  Greed will not let you make a move that would have")?;
        term.addstr(12, 9, " placed you off the grid or over a previously eaten square")?;
        term.addstr(13, 9, " unless no valid moves exist, in which case your game ends.")?;
        term.addstr(14, 9, " Other Greed commands are 'Ctrl-L' to redraw the screen,")?;
        term.addstr(15, 9, " 'p' to toggle the highlighting of the possible moves, and")?;
        term.addstr(16, 9, " 'q' to quit.  Command line options include '-s' (scores)")?;
        term.addstr(17, 9, " and '-r' to set the RNG seed.")?;
        term.move_to(18, 71)?;
        term.flush()?;
        let _ = read_key()?;
        self.render(term, attribs)
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{}: {}", cmdname(), err);
        process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let mut config = parse_args();

    if (config.height + 1) / 2 < config.maxstep {
        config.maxstep = (config.height + 1) / 2;
    }
    if (config.width + 1) / 2 < config.maxstep {
        config.maxstep = (config.width + 1) / 2;
    }
    config.status_row = config.height + 1;

    let seed = parse_seed().unwrap_or_else(default_seed);
    let mut rng = Lrand48::new(seed);

    let mut term = Terminal::enter()?;
    term.clear_screen()?;
    let attribs = default_attribs(&mut config);
    let mut game = Game::new(config, &mut rng)?;
    game.render(&mut term, &attribs)?;

    loop {
        match game.handle_key(read_key()?, &mut term, &attribs)? {
            MoveResult::Continue => {}
            MoveResult::Dead => {
                game.botmsg(&mut term, "Hit any key..", false)?;
                let _ = read_key()?;
                break;
            }
            MoveResult::Quit => break,
        }
    }

    drop(term);
    println!("\n");
    topscores(game.score, &game.config);
    Ok(())
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let mut config = Config::default();
    let mut argi = 1;

    while argi < args.len() {
        let arg = &args[argi];
        let bytes = arg.as_bytes();
        let opt = if bytes.first() == Some(&b'-') && bytes.len() >= 2 {
            bytes[1]
        } else {
            0
        };

        match opt {
            b'w' | b'h' | b'm' => {
                let optarg = option_argument(&args, &mut argi, arg);
                let optval = parse_decimal_i32(&optarg);
                match opt {
                    b'w' if optval > 1 => config.width = optval,
                    b'h' if optval > 1 => config.height = optval,
                    b'm' if optval > 0 && optval < 9 => config.maxstep = optval,
                    _ => {}
                }
            }
            b'f' => fit_to_tty(&mut config),
            b'r' => {
                let _ = option_argument(&args, &mut argi, arg);
            }
            b'p' => config.allmoves = true,
            b's' => {
                topscores(0, &config);
                process::exit(0);
            }
            _ => usage(),
        }

        argi += 1;
    }

    config
}

fn parse_seed() -> Option<i64> {
    let args: Vec<String> = env::args().collect();
    let mut argi = 1;
    while argi < args.len() {
        let arg = &args[argi];
        if arg.starts_with("-r") {
            let seed = if arg.len() == 2 {
                argi += 1;
                args.get(argi).cloned()
            } else {
                Some(arg[2..].to_string())
            };
            return seed.and_then(|value| value.parse::<i64>().ok());
        }
        argi += 1;
    }
    None
}

fn option_argument(args: &[String], argi: &mut usize, arg: &str) -> String {
    if arg.len() == 2 {
        *argi += 1;
        if *argi >= args.len() {
            usage();
        }
        args[*argi].clone()
    } else {
        arg[2..].to_string()
    }
}

fn parse_decimal_i32(text: &str) -> i32 {
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        text.parse::<i32>().unwrap_or(-1)
    } else {
        -1
    }
}

fn usage() -> ! {
    eprintln!(
        "{}\nUsage: {} [-s] [-p] [-f] [-w{{width}}] [-h{{height}}] [-m{{maxstep}}] [-r{{seed}}]",
        version(),
        cmdname()
    );
    process::exit(1);
}

fn fit_to_tty(config: &mut Config) {
    let Some((rows, cols)) = terminal_size() else {
        return;
    };
    if cols > 1 {
        config.width = cols - 1;
    }
    if rows > 3 {
        config.height = rows - 3;
    }
}

fn terminal_size() -> Option<(i32, i32)> {
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut fields = text.split_whitespace();
    let rows = fields.next()?.parse::<i32>().ok()?;
    let cols = fields.next()?.parse::<i32>().ok()?;
    Some((rows, cols))
}

fn stty_state() -> Option<String> {
    let output = Command::new("stty")
        .arg("-g")
        .stdin(Stdio::inherit())
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn read_key() -> io::Result<Key> {
    let mut byte = [0];
    io::stdin().read_exact(&mut byte)?;
    match byte[0] {
        b'\x1b' => read_escape_key(),
        0x03 => Ok(Key::Ctrl('c')),
        0x0c => Ok(Key::Ctrl('l')),
        0x12 => Ok(Key::Ctrl('r')),
        0x1c => Ok(Key::Ctrl('\\')),
        byte if byte.is_ascii() => Ok(Key::Char(byte as char)),
        _ => Ok(Key::Unknown),
    }
}

fn read_escape_key() -> io::Result<Key> {
    let mut seq = [0; 2];
    if io::stdin().read_exact(&mut seq).is_err() {
        return Ok(Key::Unknown);
    }
    match seq {
        [b'[', b'A'] | [b'O', b'A'] => Ok(Key::Up),
        [b'[', b'B'] | [b'O', b'B'] => Ok(Key::Down),
        [b'[', b'C'] | [b'O', b'C'] => Ok(Key::Right),
        [b'[', b'D'] | [b'O', b'D'] => Ok(Key::Left),
        _ => Ok(Key::Unknown),
    }
}

fn draw_box(term: &mut Terminal, top: i32, left: i32, height: i32, width: i32) -> io::Result<()> {
    term.addch(top, left, '+')?;
    term.addch(top, left + width - 1, '+')?;
    term.addch(top + height - 1, left, '+')?;
    term.addch(top + height - 1, left + width - 1, '+')?;

    for col in left + 1..left + width - 1 {
        term.addch(top, col, '-')?;
        term.addch(top + height - 1, col, '-')?;
    }
    for row in top + 1..top + height - 1 {
        term.addch(row, left, '|')?;
        term.addch(row, left + width - 1, '|')?;
        term.addstr(row, left + 1, &" ".repeat((width - 2) as usize))?;
    }
    Ok(())
}

fn default_attribs(config: &mut Config) -> [CellAttr; 10] {
    let mut attribs = [CellAttr::default(); 10];
    attribs[0] = CellAttr {
        color: Some(Color::White),
        bold: true,
    };
    attribs[1] = CellAttr {
        color: Some(Color::Yellow),
        bold: false,
    };
    attribs[2] = CellAttr {
        color: Some(Color::Red),
        bold: false,
    };
    attribs[3] = CellAttr {
        color: Some(Color::Green),
        bold: false,
    };
    attribs[4] = CellAttr {
        color: Some(Color::Cyan),
        bold: false,
    };
    attribs[5] = CellAttr {
        color: Some(Color::Magenta),
        bold: false,
    };
    attribs[6] = CellAttr {
        color: Some(Color::Yellow),
        bold: true,
    };
    attribs[7] = CellAttr {
        color: Some(Color::Red),
        bold: true,
    };
    attribs[8] = CellAttr {
        color: Some(Color::Green),
        bold: true,
    };
    attribs[9] = CellAttr {
        color: Some(Color::Cyan),
        bold: true,
    };

    if let Ok(options) = env::var("GREEDOPTS") {
        parse_greedopts(&options, config, &mut attribs);
    }

    attribs
}

fn parse_greedopts(options: &str, config: &mut Config, attribs: &mut [CellAttr; 10]) {
    let bytes = options.as_bytes();
    let mut pos = 0;
    let mut idx = 1;
    while pos < bytes.len() && bytes[pos] != b':' && idx <= 9 {
        if bytes[pos] != b' ' {
            if let Some(color) = color_from_byte(bytes[pos]) {
                attribs[idx] = CellAttr {
                    color: Some(color),
                    bold: bytes[pos].is_ascii_uppercase(),
                };
            }
        }
        pos += 1;
        idx += 1;
    }

    if pos < bytes.len() && bytes[pos] == b':' {
        for byte in &bytes[pos + 1..] {
            if *byte == b'p' {
                config.allmoves = true;
            }
        }
    }
}

fn color_from_byte(byte: u8) -> Option<Color> {
    match byte.to_ascii_lowercase() {
        b'b' => Some(Color::Blue),
        b'g' => Some(Color::Green),
        b'c' => Some(Color::Cyan),
        b'r' => Some(Color::Red),
        b'm' => Some(Color::Magenta),
        b'y' => Some(Color::Yellow),
        b'w' => Some(Color::White),
        _ => None,
    }
}

fn score_perc(score: i32, height: i32, width: i32) -> f32 {
    if score <= 0 {
        return 0.0;
    }
    let total = height * width - 1;
    if score >= total {
        return 100.0;
    }
    (score as f32 / total as f32) * 100.0
}

fn score_adj(score: i32, height: i32, width: i32, maxstep: i32) -> f32 {
    if maxstep <= 0 {
        return 0.0;
    }
    let score_p = score_perc(score, height, width);
    if maxstep >= 9 {
        score_p
    } else {
        score_p * maxstep as f32 / 9.0
    }
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

fn score_layout() -> ScoreLayout {
    let time_offset = align_up(USER_FIELD_LEN, align_of::<TimeT>());
    let height_offset = align_up(time_offset + size_of::<TimeT>(), align_of::<i32>());
    let width_offset = height_offset + size_of::<i32>();
    let maxstep_offset = width_offset + size_of::<i32>();
    let score_offset = maxstep_offset + size_of::<i32>();
    let size = align_up(
        score_offset + size_of::<i32>(),
        align_of::<TimeT>().max(align_of::<i32>()),
    );

    ScoreLayout {
        size,
        time_offset,
        height_offset,
        width_offset,
        maxstep_offset,
        score_offset,
    }
}

fn read_i32(record: &[u8], offset: usize) -> i32 {
    let mut bytes = [0; size_of::<i32>()];
    let len = bytes.len();
    bytes.copy_from_slice(&record[offset..offset + len]);
    i32::from_ne_bytes(bytes)
}

fn write_i32(record: &mut [u8], offset: usize, value: i32) {
    let bytes = value.to_ne_bytes();
    record[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

fn read_time(record: &[u8], offset: usize) -> TimeT {
    let mut bytes = [0; size_of::<TimeT>()];
    let len = bytes.len();
    bytes.copy_from_slice(&record[offset..offset + len]);
    TimeT::from_ne_bytes(bytes)
}

fn write_time(record: &mut [u8], offset: usize, value: TimeT) {
    let bytes = value.to_ne_bytes();
    record[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

fn read_score(record: &[u8], layout: &ScoreLayout) -> Score {
    let mut score = Score::default();
    score.user.copy_from_slice(&record[..USER_FIELD_LEN]);
    score.time = read_time(record, layout.time_offset);
    score.height = read_i32(record, layout.height_offset);
    score.width = read_i32(record, layout.width_offset);
    score.maxstep = read_i32(record, layout.maxstep_offset);
    score.score = read_i32(record, layout.score_offset);
    score
}

fn write_score(record: &mut [u8], layout: &ScoreLayout, score: Score) {
    record[..USER_FIELD_LEN].copy_from_slice(&score.user);
    write_time(record, layout.time_offset, score.time);
    write_i32(record, layout.height_offset, score.height);
    write_i32(record, layout.width_offset, score.width);
    write_i32(record, layout.maxstep_offset, score.maxstep);
    write_i32(record, layout.score_offset, score.score);
}

fn read_score_file(file: &mut File, layout: &ScoreLayout) -> [Score; MAXSCORES] {
    let mut toplist = [Score::default(); MAXSCORES];
    let mut bytes = vec![0; layout.size * MAXSCORES];
    let _ = file.read(&mut bytes);
    for (idx, entry) in toplist.iter_mut().enumerate() {
        let start = idx * layout.size;
        let end = start + layout.size;
        *entry = read_score(&bytes[start..end], layout);
    }
    toplist
}

fn write_score_file(file: &mut File, layout: &ScoreLayout, toplist: &[Score; MAXSCORES]) {
    let mut bytes = vec![0; layout.size * MAXSCORES];
    for (idx, score) in toplist.iter().enumerate() {
        let start = idx * layout.size;
        let end = start + layout.size;
        write_score(&mut bytes[start..end], layout, *score);
    }
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.write_all(&bytes);
}

fn current_user() -> UserInfo {
    let name = env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    UserInfo {
        name,
        home: PathBuf::from(home),
    }
}

fn open_score_path(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

fn open_score_file(user: &UserInfo) -> File {
    if let Ok(file) = open_score_path(Path::new(scorefile())) {
        return file;
    }

    let local = user.home.join(LOCALSCOREFILE);
    match open_score_path(&local) {
        Ok(file) => file,
        Err(_) => {
            eprintln!("{}: ~/{}: Cannot open.", cmdname(), LOCALSCOREFILE);
            process::exit(1);
        }
    }
}

fn lockit(on: bool) {
    if on {
        let mut attempt = 1;
        loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0)
                .open(LOCKPATH)
            {
                Ok(_) => break,
                Err(_) => {
                    println!("Waiting for scorefile access... {}/15", attempt);
                    if attempt >= 15 {
                        println!("Overriding stale lock...");
                        if remove_file(LOCKPATH).is_err() {
                            eprintln!("{}: {}: Can't unlink lock.", cmdname(), LOCKPATH);
                            process::exit(1);
                        }
                    }
                    attempt += 1;
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    } else {
        let _ = remove_file(LOCKPATH);
    }
}

fn set_score_user(score: &mut Score, user: &str) {
    score.user = [0; USER_FIELD_LEN];
    let bytes = user.as_bytes();
    let len = bytes.len().min(USERNAMELEN);
    score.user[..len].copy_from_slice(&bytes[..len]);
}

fn score_user(score: &Score) -> String {
    let len = score
        .user
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(USER_FIELD_LEN);
    String::from_utf8_lossy(&score.user[..len]).into_owned()
}

fn topscores(newscore: i32, config: &Config) {
    let user = current_user();
    let mut file = open_score_file(&user);
    let layout = score_layout();

    lockit(true);
    let mut toplist = read_score_file(&mut file, &layout);
    let mut new_index = None;

    if newscore > 0 {
        let newscore_p = score_adj(newscore, config.height, config.width, config.maxstep);
        for (idx, existing) in toplist.iter().enumerate() {
            if newscore_p > score_adj(existing.score, existing.height, existing.width, existing.maxstep) {
                new_index = Some(idx);
                break;
            }
        }

        if let Some(idx) = new_index {
            for move_idx in (idx + 1..MAXSCORES).rev() {
                toplist[move_idx] = toplist[move_idx - 1];
            }

            let mut score = Score {
                score: newscore,
                time: current_time(),
                height: config.height,
                width: config.width,
                maxstep: config.maxstep,
                ..Score::default()
            };
            set_score_user(&mut score, &user.name);
            toplist[idx] = score;
            write_score_file(&mut file, &layout, &toplist);
        }
    }

    drop(file);
    lockit(false);

    if toplist[0].score == 0 {
        println!("No high scores.");
    }

    for (idx, entry) in toplist.iter().enumerate() {
        if entry.score == 0 {
            break;
        }
        if Some(idx) == new_index {
            print!("\x1b[7m");
        }

        let sizestr = if entry.maxstep < 9 {
            format!("({}x{}:{})", entry.width, entry.height, entry.maxstep)
        } else {
            format!("({}x{})", entry.width, entry.height)
        };

        println!(
            "{:<5} {} {:>6} {:<10} {:>6.2}% {}",
            idx + 1,
            format_time(entry.time),
            entry.score,
            sizestr,
            score_perc(entry.score, entry.height, entry.width),
            score_user(entry)
        );

        if Some(idx) == new_index {
            print!("\x1b[0m");
        }
    }
}

fn current_time() -> TimeT {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as TimeT)
        .unwrap_or(0)
}

fn default_seed() -> i64 {
    let pid = process::id() as i64;
    current_time() ^ (pid << 16)
}

fn format_time(timestamp: TimeT) -> String {
    let output = Command::new("date")
        .arg("-d")
        .arg(format!("@{}", timestamp))
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    timestamp.to_string()
}

fn version() -> String {
    format!("Greed v{}", release())
}

fn release() -> &'static str {
    option_env!("RELEASE").unwrap_or(DEFAULT_RELEASE)
}

fn scorefile() -> &'static str {
    option_env!("SCOREFILE").unwrap_or(DEFAULT_SCOREFILE)
}

fn cmdname() -> String {
    env::args().next().unwrap_or_else(|| "greed".to_string())
}

/* the end */
