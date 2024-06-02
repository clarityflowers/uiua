//! Pretty printing Uiua arrays

use std::{
    f64::{
        consts::{PI, TAU},
        INFINITY,
    },
    iter::once,
    mem::take,
};

use crate::{
    algorithm::map::{EMPTY_NAN, TOMBSTONE_NAN},
    array::{Array, ArrayValue},
    boxed::Boxed,
    value::Value,
    Complex, Primitive, WILDCARD_CHAR, WILDCARD_NAN,
};

type Grid<T = char> = Vec<Vec<T>>;
type Metagrid = Grid<Grid>;

#[derive(Debug, Clone, Copy, Default)]
pub struct GridFmtParams {
    pub boxed: bool,
    pub label: bool,
}

pub trait GridFmt {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid;
    fn grid_string(&self, label: bool) -> String {
        let mut s: String = self
            .fmt_grid(GridFmtParams {
                label,
                ..Default::default()
            })
            .into_iter()
            .flat_map(|v| v.into_iter().chain(once('\n')))
            .collect();
        s.pop();
        s
    }
}

fn boxed_scalar(boxed: bool) -> impl Iterator<Item = char> {
    boxed.then_some(Primitive::Box.glyph().unwrap()).into_iter()
}

impl GridFmt for u8 {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        let boxed = boxed_scalar(params.boxed);
        vec![boxed.chain(self.to_string().chars()).collect()]
    }
}

impl GridFmt for f64 {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        let f = *self;
        let positive = f.abs();
        let is_neg = f < 0.0;
        let minus = if is_neg { "¯" } else { "" };
        let s = if (positive - PI).abs() < f64::EPSILON {
            format!("{minus}π")
        } else if (positive - TAU).abs() < f64::EPSILON {
            format!("{minus}τ")
        } else if (positive - PI / 2.0).abs() < f64::EPSILON {
            format!("{minus}η")
        } else if positive == INFINITY {
            format!("{minus}∞")
        } else if f.to_bits() == EMPTY_NAN.to_bits() || f.to_bits() == TOMBSTONE_NAN.to_bits() {
            return vec![vec!['⋅']];
        } else if f.to_bits() == WILDCARD_NAN.to_bits() {
            return vec![vec!['W']];
        } else if positive.fract() == 0.0 || positive.is_nan() {
            format!("{minus}{positive}")
        } else {
            let mut pos_formatted = positive.to_string();
            if pos_formatted.len() >= 17 {
                let mut consecutive_start = 0;
                let mut consecutive_len = 0;
                let mut hit_decimal = false;
                for (i, c) in pos_formatted.chars().enumerate() {
                    if c == '.' {
                        hit_decimal = true;
                    } else if !hit_decimal {
                        continue;
                    }
                    let local_len = pos_formatted
                        .chars()
                        .skip(i + 1)
                        .take_while(|&d| d == c)
                        .count();
                    if local_len > consecutive_len {
                        consecutive_start = i;
                        consecutive_len = local_len;
                    }
                }
                if consecutive_len >= 5 {
                    if consecutive_start + consecutive_len + 1 == pos_formatted.len() {
                        pos_formatted.replace_range(consecutive_start + 3.., "…")
                    } else {
                        pos_formatted.replace_range(
                            consecutive_start + 2..consecutive_start + consecutive_len,
                            "…",
                        )
                    }
                }
            }
            if is_neg {
                format!("{minus}{pos_formatted}")
            } else {
                pos_formatted
            }
        };
        vec![boxed_scalar(params.boxed).chain(s.chars()).collect()]
    }
}

impl GridFmt for Complex {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        if self.im.abs() == 0.0 {
            self.re.fmt_grid(params)
        } else if self.re.abs() == 0.0 {
            if self.im == 1.0 {
                vec![boxed_scalar(params.boxed).chain(['i']).collect()]
            } else if self.im == -1.0 {
                vec![boxed_scalar(params.boxed).chain(['-', 'i']).collect()]
            } else {
                let mut grid = self.im.fmt_grid(params);
                grid[0].push('i');
                grid
            }
        } else {
            let mut re = self.re.fmt_grid(params);
            let im = if self.im.abs() == 1.0 {
                String::new()
            } else {
                self.im.abs().grid_string(params.label)
            };
            let sign = if self.im < 0.0 { '-' } else { '+' };
            re[0].push(sign);
            re[0].extend(im.chars());
            re[0].push('i');
            re
        }
    }
}

impl GridFmt for Value {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        'box_list: {
            let Value::Box(b) = self else {
                break 'box_list;
            };
            if b.rank() != 1 || b.meta().map_keys.is_some() {
                break 'box_list;
            }
            let mut item_lines = Vec::new();
            for Boxed(val) in &b.data {
                let grid = val.fmt_grid(GridFmtParams {
                    boxed: false,
                    ..params
                });
                if grid.len() == 1 {
                    item_lines.push(grid.into_iter().next().unwrap());
                } else {
                    break 'box_list;
                }
            }
            let mut only_row = Vec::new();
            if let Some(label) = &b.meta().label {
                only_row.extend(label.chars());
                only_row.push(':');
                only_row.push(' ');
            }
            only_row.push('{');
            for (i, line) in item_lines.into_iter().enumerate() {
                if i > 0 {
                    only_row.push(' ');
                }
                only_row.extend(line);
            }
            only_row.push('}');
            return vec![only_row];
        }
        match self {
            Value::Num(n) => n.fmt_grid(params),
            Value::Byte(b) => b.fmt_grid(params),
            Value::Complex(c) => c.fmt_grid(params),
            Value::Box(v) => v.fmt_grid(params),
            Value::Char(c) => c.fmt_grid(params),
        }
    }
}

pub fn format_char_inner(c: char) -> String {
    match c {
        char::MAX => return '_'.to_string(),
        WILDCARD_CHAR => return '�'.to_string(),
        _ => {}
    }
    let formatted = format!("{c:?}");
    if c == '\'' {
        "'".to_string()
    } else if formatted.starts_with("'\\u{") {
        let n = c as u32;
        if n < 128 {
            format!("\\x{n:02x}")
        } else if n < 16u32.pow(4) {
            format!("\\u{n:04x}")
        } else {
            format!("\\u{{{n:x}}}")
        }
    } else {
        formatted[1..formatted.len() - 1].to_string()
    }
}

impl GridFmt for char {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        vec![once(if params.boxed { '⌞' } else { '@' })
            .chain(format_char_inner(*self).chars())
            .collect()]
    }
}

impl GridFmt for Boxed {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        let subparams = GridFmtParams {
            boxed: true,
            ..params
        };
        let mut grid = match self.as_value() {
            Value::Num(array) => array.fmt_grid(subparams),
            Value::Byte(array) => array.fmt_grid(subparams),
            Value::Complex(array) => array.fmt_grid(subparams),
            Value::Char(array) => array.fmt_grid(subparams),
            Value::Box(array) => array.fmt_grid(subparams),
        };
        if params.boxed && grid.len() == 1 {
            grid = vec![boxed_scalar(true)
                .chain(grid.into_iter().flatten())
                .collect()];
        }
        grid
    }
}

impl<T: GridFmt + ArrayValue> GridFmt for Array<T> {
    fn fmt_grid(&self, params: GridFmtParams) -> Grid {
        let mut metagrid: Option<Metagrid> = None;
        let mut grid = if let Some(pointer) = self.meta().pointer.filter(|p| p.raw) {
            vec![boxed_scalar(params.boxed)
                .chain(format!("0x{:x}", pointer.ptr).chars())
                .collect()]
        } else if self.shape.is_empty() && !self.is_map() {
            // Scalar
            self.data[0].fmt_grid(params)
        } else if self.shape == [0] && !self.is_map() {
            // Empty list
            let (left, right) = T::grid_fmt_delims(params.boxed);
            let inner = T::empty_list_inner();
            let mut row = vec![left];
            row.extend(inner.chars());
            row.push(right);
            vec![row]
        } else {
            // Hashmap
            if let Some(keys) = &self.meta().map_keys {
                let metagrid = metagrid.get_or_insert_with(Metagrid::new);
                let params = GridFmtParams {
                    boxed: false,
                    ..params
                };
                for (key, value) in self.map_kv() {
                    let key = key.fmt_grid(params);
                    let value = value.fmt_grid(params);
                    metagrid.push(vec![key, vec![" → ".chars().collect()], value]);
                }
                if metagrid.is_empty() {
                    let mut keys_row_shape = keys.keys.shape().clone();
                    keys_row_shape.make_row();
                    let mut row = match &keys.keys {
                        Value::Num(_) => shape_row::<f64>(&keys_row_shape),
                        Value::Byte(_) => shape_row::<u8>(&keys_row_shape),
                        Value::Complex(_) => shape_row::<Complex>(&keys_row_shape),
                        Value::Char(_) => shape_row::<char>(&keys_row_shape),
                        Value::Box(_) => shape_row::<Boxed>(&keys_row_shape),
                    };
                    row.extend([' ', '→', ' ']);
                    let mut value_row_shape = self.shape.clone();
                    value_row_shape.make_row();
                    row.extend(shape_row::<T>(&value_row_shape));
                    metagrid.push(vec![vec![row]]);
                }
            }

            // Default array formatting
            let mut metagrid = metagrid.unwrap_or_else(|| {
                let mut metagrid = Metagrid::new();
                fmt_array(&self.shape, &self.data, params, &mut metagrid);
                metagrid
            });

            // Synthesize a grid from the metagrid
            let mut grid: Grid = Grid::new();

            // Determine max row heights and column widths
            let metagrid_width = metagrid.iter().map(|row| row.len()).max().unwrap();
            let metagrid_height = metagrid.len();
            let mut column_widths = vec![0; metagrid_width];
            let mut row_heights = vec![0; metagrid_height];
            for row in 0..metagrid_height {
                let max_row_height = metagrid[row]
                    .iter()
                    .map(|cell| cell.len())
                    .max()
                    .unwrap_or(1);
                row_heights[row] = max_row_height;
            }
            for col in 0..metagrid_width {
                let max_col_width = metagrid
                    .iter_mut()
                    .flat_map(|row| row.get(col)?.iter().map(|cell| cell.len()).max())
                    .max()
                    .unwrap_or(0);
                column_widths[col] = max_col_width;
            }
            // Pad each metagrid cell to its row's max height and column's max width
            for row in 0..metagrid_height {
                let row_height = row_heights[row];
                let mut subrows = vec![vec![]; row_height];
                for (col_width, cell) in column_widths.iter().zip(&mut metagrid[row]) {
                    pad_grid_center(*col_width, row_height, true, cell);
                    for (subrow, cell_row) in subrows.iter_mut().zip(take(cell)) {
                        subrow.extend(cell_row);
                    }
                }
                grid.extend(subrows);
            }
            // Outline the grid
            let grid_row_count = grid.len();
            if grid_row_count == 1 && self.rank() == 1 {
                // Add brackets to lists
                let (left, right) = T::grid_fmt_delims(params.boxed);
                grid[0].insert(0, left);
                grid[0].push(right);
            } else {
                // Add corners to non-vectors
                let width = grid[0].len();
                let height = grid.len();
                pad_grid_center(
                    width + 4,
                    (height + 2).max(self.rank() + 1),
                    false,
                    &mut grid,
                );
                grid[0][0] = if params.boxed { '╓' } else { '╭' };
                grid[0][1] = '─';
                for i in 0..self.rank().saturating_sub(1) {
                    grid[i + 1][0] = if params.boxed { '╟' } else { '╷' };
                }
                *grid.last_mut().unwrap().last_mut().unwrap() =
                    if params.boxed { '╜' } else { '╯' };
            }
            grid
        };

        // Add handle kind
        if let Some(kind) = &self.meta().handle_kind {
            if grid.len() == 1 {
                grid[0] = (kind.to_string().chars().chain(['(']))
                    .chain(take(&mut grid[0]))
                    .chain([')'])
                    .collect();
            }
        }

        // Add label
        if params.label {
            if let Some(label) = &self.meta().label {
                if grid.len() == 1 {
                    grid[0] = (label.chars().chain([':', ' ']))
                        .chain(take(&mut grid[0]))
                        .collect();
                } else {
                    grid[0].truncate(2);
                    grid[0].push(' ');
                    grid[0].extend(label.chars());
                    while grid[0].len() < grid[1].len() {
                        grid[0].push(' ');
                    }
                }
            }
        }

        // Add pointer
        if let Some(pointer) = self.meta().pointer.filter(|p| !p.raw) {
            if grid.len() == 1 {
                grid[0].extend(format!("(0x{:x})", pointer.ptr).chars());
            }
        }

        // Handle really big grid
        if self.rank() > 1 {
            let max_width = term_size::dimensions().map_or(1000, |(w, _)| w);
            for row in grid.iter_mut() {
                if row.len() > max_width {
                    let diff = row.len() - max_width;
                    row.truncate(max_width);
                    if !(row[max_width - 1].is_whitespace() && diff == 1)
                        && (2..4).any(|i| !row[max_width - i].is_whitespace())
                    {
                        row[max_width - 1] = '…';
                    }
                }
            }
        }

        grid
    }
}

impl<T: ArrayValue> Array<T> {
    /// Get a string representation of the shape of the array
    pub fn shape_string(&self) -> String {
        let base: String = shape_row::<T>(&self.shape).into_iter().collect();
        if let Some(keys) = &self.meta().map_keys {
            let mut keys_shape = keys.keys.shape().clone();
            keys_shape[0] = self.row_count();
            let mut s: String = match keys.keys {
                Value::Num(_) => shape_row::<f64>(&keys_shape),
                Value::Byte(_) => shape_row::<u8>(&keys_shape),
                Value::Complex(_) => shape_row::<Complex>(&keys_shape),
                Value::Char(_) => shape_row::<char>(&keys_shape),
                Value::Box(_) => shape_row::<Boxed>(&keys_shape),
            }
            .into_iter()
            .collect();
            s.push_str(" → ");
            s.push_str(&base);
            s
        } else {
            base
        }
    }
}

impl Value {
    /// Get a string representation of the shape of the value
    pub fn shape_string(&self) -> String {
        self.generic_ref(
            Array::shape_string,
            Array::shape_string,
            Array::shape_string,
            Array::shape_string,
            Array::shape_string,
        )
    }
}

fn shape_row<T: ArrayValue>(shape: &[usize]) -> Vec<char> {
    let mut shape_row = Vec::new();
    for (i, dim) in shape.iter().enumerate() {
        if i > 0 {
            shape_row.extend("×".chars());
        }
        shape_row.extend(dim.to_string().chars());
    }
    if !shape.is_empty() {
        shape_row.push(' ');
    }
    shape_row.push(T::SYMBOL);
    shape_row
}

fn fmt_array<T: GridFmt + ArrayValue>(
    shape: &[usize],
    data: &[T],
    params: GridFmtParams,
    metagrid: &mut Metagrid,
) {
    if data.is_empty() {
        metagrid.push(vec![vec![shape_row::<T>(shape)]]);
        return;
    }
    let rank = shape.len();
    if rank == 0 {
        metagrid.push(vec![data[0].fmt_grid(GridFmtParams {
            boxed: false,
            ..params
        })]);
        return;
    }
    if rank == 1 {
        let mut row = Vec::with_capacity(shape[0]);
        if T::compress_list_grid() {
            let s: String = data
                .iter()
                .map(|c| c.to_string())
                .collect::<String>()
                .chars()
                .map(format_char_inner)
                .collect();
            row.push(vec![s.chars().collect()]);
        } else {
            for (i, val) in data.iter().enumerate() {
                let mut grid = val.fmt_grid(GridFmtParams {
                    boxed: false,
                    ..params
                });
                if i > 0 {
                    pad_grid_min(grid[0].len() + 1, grid.len(), &mut grid)
                }
                row.push(grid);
            }
        }
        metagrid.push(row);
        return;
    }
    let cell_count = shape[0];
    if cell_count == 0 {
        metagrid.push(vec![vec![vec![' ']]]);
        return;
    }
    let row_shape = &shape[1..];
    let cell_size = data.len() / cell_count;
    let row_height: usize = row_shape.iter().rev().skip(1).product();
    let max_height = if term_size::dimensions().is_some() {
        100
    } else {
        300
    };
    for (i, cell) in data.chunks(cell_size).enumerate() {
        if i > 0 && rank > 2 {
            for _ in 0..rank - 2 {
                metagrid.push(vec![vec![vec![' ']]; metagrid.last().unwrap().len()]);
            }
        }
        fmt_array(row_shape, cell, params, metagrid);
        if T::compress_list_grid() && rank == 2 {
            let (left, right) = T::grid_fmt_delims(false);
            for grid in metagrid.last_mut().unwrap() {
                for row in grid.iter_mut() {
                    row.insert(0, left);
                    row.push(right);
                }
            }
        }
        if i * row_height >= max_height {
            let mut elipses_row = Vec::new();
            for prev_grid in metagrid.last().unwrap() {
                let prev_row = &prev_grid[0];
                let mut new_row = Vec::with_capacity(prev_row.len());
                for c in prev_row {
                    new_row.push(if c.is_whitespace() { ' ' } else { '⋮' });
                }
                elipses_row.push(vec![new_row]);
            }
            metagrid.push(elipses_row);
            break;
        }
    }
}

fn pad_grid_center(width: usize, height: usize, align: bool, grid: &mut Grid) {
    grid.truncate(height);
    if grid.len() < height {
        let diff = height - grid.len();
        let post_pad = diff / 2;
        let pre_pad = diff - post_pad;
        for _ in 0..pre_pad {
            grid.insert(0, vec![' '; width]);
        }
        for _ in 0..post_pad {
            grid.push(vec![' '; width]);
        }
    }
    for row in grid.iter_mut() {
        row.truncate(width);
        if row.len() < width {
            let no_left = row.strip_prefix(&[' ']).unwrap_or(row);
            let diff = width - row.len();
            let (pre_pad, post_pad) = if align
                && row
                    .last()
                    .is_some_and(|c| c.is_ascii_digit() || "ηπτ".contains(*c))
            {
                (diff, 0)
            } else if align && (no_left.starts_with(&['⟦']) || no_left.starts_with(&['⌜'])) {
                (0, diff)
            } else {
                let post = (diff + 1) / 2;
                (diff - post, post)
            };
            for _ in 0..pre_pad {
                row.insert(0, ' ');
            }
            for _ in 0..post_pad {
                row.push(' ');
            }
        }
    }
}

fn pad_grid_min(width: usize, height: usize, grid: &mut Grid) {
    grid.truncate(height);
    while grid.len() < height {
        grid.insert(0, vec![' '; width]);
    }
    for row in grid.iter_mut() {
        row.truncate(width);
        while row.len() < width {
            row.insert(0, ' ');
        }
    }
}
