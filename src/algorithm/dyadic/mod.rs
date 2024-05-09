//! Algorithms for dyadic array operations

mod combine;
mod structure;

use std::{
    cmp::Ordering,
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    iter::{once, repeat},
    mem::take,
};

use ecow::{eco_vec, EcoVec};
use rayon::prelude::*;

use crate::{
    algorithm::pervade::{self, bin_pervade_recursive, InfalliblePervasiveFn},
    array::*,
    boxed::Boxed,
    cowslice::{cowslice, CowSlice},
    value::Value,
    Shape, Uiua, UiuaResult,
};

use super::{shape_prefixes_match, ArrayCmpSlice, FillContext};

impl Value {
    pub(crate) fn bin_coerce_to_boxes<T, C: FillContext, E: ToString>(
        self,
        other: Self,
        ctx: &C,
        on_success: impl FnOnce(Array<Boxed>, Array<Boxed>, &C) -> Result<T, C::Error>,
        on_error: impl FnOnce(&str, &str) -> E,
    ) -> Result<T, C::Error> {
        match (self, other) {
            (Value::Box(a), Value::Box(b)) => on_success(a, b, ctx),
            (Value::Box(a), b) => on_success(a, b.coerce_to_boxes(), ctx),
            (a, Value::Box(b)) => on_success(a.coerce_to_boxes(), b, ctx),
            (a, b) => Err(ctx.error(on_error(a.type_name(), b.type_name()))),
        }
    }
    pub(crate) fn bin_coerce_to_boxes_mut<T, C: FillContext, E: ToString>(
        &mut self,
        other: Self,
        ctx: &C,
        on_success: impl FnOnce(&mut Array<Boxed>, Array<Boxed>, &C) -> Result<T, C::Error>,
        on_error: impl FnOnce(&str, &str) -> E,
    ) -> Result<T, C::Error> {
        match (self, other) {
            (Value::Box(a), Value::Box(b)) => on_success(a, b, ctx),
            (Value::Box(a), b) => on_success(a, b.coerce_to_boxes(), ctx),
            (a, Value::Box(b)) => {
                let mut a_arr = take(a).coerce_to_boxes();
                let res = on_success(&mut a_arr, b, ctx)?;
                *a = a_arr.into();
                Ok(res)
            }
            (a, b) => Err(ctx.error(on_error(a.type_name(), b.type_name()))),
        }
    }
}

impl<T: Clone + std::fmt::Debug> Array<T> {
    pub(crate) fn depth_slices<U: Clone + std::fmt::Debug, C: FillContext>(
        &mut self,
        other: &Array<U>,
        mut a_depth: usize,
        mut b_depth: usize,
        ctx: &C,
        mut f: impl FnMut(&[usize], &mut [T], &[usize], &[U], &C) -> Result<(), C::Error>,
    ) -> Result<(), C::Error> {
        let a = self;
        let mut b = other;
        let mut local_b;
        a_depth = a_depth.min(a.rank());
        b_depth = b_depth.min(b.rank());
        let a_prefix = &a.shape[..a_depth];
        let b_prefix = &b.shape[..b_depth];
        if !a_prefix.iter().zip(b_prefix).all(|(a, b)| a == b) {
            while a.shape.starts_with(&[1]) {
                if a_depth == 0 {
                    break;
                }
                a.shape.remove(0);
                a_depth -= 1;
            }
            if b.shape.starts_with(&[1]) {
                local_b = b.clone();
                while local_b.shape.starts_with(&[1]) {
                    if b_depth == 0 {
                        break;
                    }
                    local_b.shape.remove(0);
                    b_depth -= 1;
                }
                b = &local_b;
            }
            let a_prefix = &a.shape[..a_depth];
            let b_prefix = &b.shape[..b_depth];
            if !a_prefix.iter().zip(b_prefix).all(|(a, b)| a == b) {
                return Err(ctx.error(format!(
                    "Cannot combine arrays with shapes {} and {} \
                    because shape prefixes {} and {} are not compatible",
                    a.shape(),
                    b.shape(),
                    FormatShape(a_prefix),
                    FormatShape(b_prefix)
                )));
            }
        }
        match a_depth.cmp(&b_depth) {
            Ordering::Equal => {}
            Ordering::Less => {
                for b_dim in b.shape[..b_depth - a_depth].iter().rev() {
                    a.reshape_scalar(Ok(*b_dim as isize));
                    a_depth += 1;
                }
            }
            Ordering::Greater => {
                for a_dim in a.shape[..a_depth - b_depth].iter().rev() {
                    local_b = b.clone();
                    local_b.reshape_scalar(Ok(*a_dim as isize));
                    b = &local_b;
                    b_depth += 1;
                }
            }
        }

        let a_row_shape = &a.shape[a_depth..];
        let b_row_shape = &b.shape[b_depth..];
        let a_row_len: usize = a_row_shape.iter().product();
        let b_row_len: usize = b_row_shape.iter().product();
        if a_row_len == 0 || b_row_len == 0 {
            return Ok(());
        }
        for (a, b) in (a.data.as_mut_slice())
            .chunks_exact_mut(a_row_len)
            .zip(b.data.as_slice().chunks_exact(b_row_len))
        {
            f(a_row_shape, a, b_row_shape, b, ctx)?;
        }
        Ok(())
    }
}

impl Value {
    /// `reshape` this value with another
    pub fn reshape(&mut self, shape: &Self, env: &Uiua) -> UiuaResult {
        let target_shape = shape.as_ints_or_infs(
            env,
            "Shape should be a single integer \
            or a list of integers or infinity",
        )?;
        if shape.rank() == 0 {
            let n = target_shape[0];
            match self {
                Value::Num(a) => a.reshape_scalar(n),
                Value::Byte(a) => a.reshape_scalar(n),
                Value::Complex(a) => a.reshape_scalar(n),
                Value::Char(a) => a.reshape_scalar(n),
                Value::Box(a) => a.reshape_scalar(n),
            }
        } else {
            match self {
                Value::Num(a) => a.reshape(&target_shape, env),
                Value::Byte(a) => {
                    if env.num_scalar_fill().is_ok() && env.byte_scalar_fill().is_err() {
                        let mut arr: Array<f64> = a.convert_ref();
                        arr.reshape(&target_shape, env)?;
                        *self = arr.into();
                        Ok(())
                    } else {
                        a.reshape(&target_shape, env)
                    }
                }
                Value::Complex(a) => a.reshape(&target_shape, env),
                Value::Char(a) => a.reshape(&target_shape, env),
                Value::Box(a) => a.reshape(&target_shape, env),
            }?
        }
        Ok(())
    }
    pub(crate) fn undo_reshape(&mut self, old_shape: &Self, env: &Uiua) -> UiuaResult {
        if old_shape.as_nat(env, "").is_ok() {
            return Err(env.error("Cannot undo scalar reshae"));
        }
        let orig_shape = old_shape.as_nats(env, "Shape should be a list of integers")?;
        if orig_shape.iter().product::<usize>() == self.shape().iter().product::<usize>() {
            *self.shape_mut() = Shape::from(orig_shape.as_slice());
            Ok(())
        } else {
            Err(env.error(format!(
                "Cannot unreshape array because its old shape was {}, \
                but its new shape is {}, which has a different number of elements",
                FormatShape(&orig_shape),
                self.shape()
            )))
        }
    }
}

impl<T: Clone> Array<T> {
    /// `reshape` this array by replicating it as the rows of a new array
    pub fn reshape_scalar(&mut self, count: Result<isize, bool>) {
        self.take_map_keys();
        match count {
            Ok(count) => {
                if count == 0 {
                    self.data.clear();
                    self.shape.insert(0, 0);
                    return;
                }
                self.data
                    .reserve((count.unsigned_abs() - 1) * self.data.len());
                let row = self.data.to_vec();
                for _ in 1..count.unsigned_abs() {
                    self.data.extend_from_slice(&row);
                }
                if count < 0 {
                    self.reverse();
                }
                self.shape.insert(0, count.unsigned_abs());
            }
            Err(rev) => {
                if rev {
                    self.reverse()
                }
            }
        }
    }
}

impl<T: ArrayValue> Array<T> {
    /// `reshape` the array
    pub fn reshape(&mut self, dims: &[Result<isize, bool>], env: &Uiua) -> UiuaResult {
        let fill = env.scalar_fill::<T>();
        let axes = derive_shape(&self.shape, dims, fill.is_ok(), env)?;
        if (axes.first()).map_or(true, |&d| d.unsigned_abs() != self.row_count()) {
            self.take_map_keys();
        }
        let reversed_axes: Vec<usize> = (axes.iter().enumerate())
            .filter_map(|(i, &s)| if s < 0 { Some(i) } else { None })
            .collect();
        let shape: Shape = axes.iter().map(|&s| s.unsigned_abs()).collect();
        let target_len: usize = shape.iter().product();
        if self.data.len() < target_len {
            match env.scalar_fill::<T>() {
                Ok(fill) => {
                    let start = self.data.len();
                    self.data.extend(repeat(fill).take(target_len - start));
                }
                Err(e) => {
                    if self.data.is_empty() {
                        if !shape.contains(&0) {
                            return Err(env
                                .error(format!(
                                    "Cannot reshape empty array without a fill value{e}"
                                ))
                                .fill());
                        }
                    } else if self.rank() == 0 {
                        self.data = cowslice![self.data[0].clone(); target_len];
                    } else {
                        let start = self.data.len();
                        let old_data = self.data.clone();
                        self.data.reserve(target_len - self.data.len());
                        let additional = target_len - start;
                        for _ in 0..additional / start {
                            self.data.extend_from_slice(&old_data);
                        }
                        self.data.extend_from_slice(&old_data[..additional % start]);
                    }
                }
            }
        } else {
            self.data.truncate(target_len);
        }
        self.shape = shape;
        self.validate_shape();
        for s in reversed_axes {
            self.reverse_depth(s);
        }
        Ok(())
    }
}

fn derive_shape(
    shape: &[usize],
    dims: &[Result<isize, bool>],
    has_fill: bool,
    env: &Uiua,
) -> UiuaResult<Vec<isize>> {
    let mut inf_count = 0;
    for dim in dims {
        if dim.is_err() {
            inf_count += 1;
        }
    }
    let derive_len = |data_len: usize, other_len: usize| {
        (if has_fill { f32::ceil } else { f32::floor }(data_len as f32 / other_len as f32) as isize)
    };
    Ok(match inf_count {
        0 => dims.iter().map(|dim| dim.unwrap()).collect(),
        1 => {
            if let Err(rev) = dims[0] {
                let rev_mul = if rev { -1 } else { 1 };
                if dims[1..].iter().any(|&dim| dim.is_err()) {
                    return Err(env.error("Cannot reshape array with multiple infinite dimensions"));
                }
                let shape_non_leading_len = dims[1..].iter().flatten().product::<isize>() as usize;
                if shape_non_leading_len == 0 {
                    return Err(env.error("Cannot reshape array with any 0 non-leading dimensions"));
                }
                let leading_len =
                    rev_mul * derive_len(shape.iter().product(), shape_non_leading_len);
                let mut axes = vec![leading_len];
                axes.extend(dims[1..].iter().flatten());
                axes
            } else if let Err(rev) = *dims.last().unwrap() {
                let rev_mul = if rev { -1 } else { 1 };
                if dims.iter().rev().skip(1).any(|&dim| dim.is_err()) {
                    return Err(env.error("Cannot reshape array with multiple infinite dimensions"));
                }
                let mut axes: Vec<isize> = dims.iter().copied().flatten().collect();
                let shape_non_trailing_len = axes.iter().copied().product::<isize>().unsigned_abs();
                if shape_non_trailing_len == 0 {
                    return Err(
                        env.error("Cannot reshape array with any 0 non-trailing dimensions")
                    );
                }
                let trailing_len =
                    rev_mul * derive_len(shape.iter().product(), shape_non_trailing_len);
                axes.push(trailing_len);
                axes
            } else {
                let inf_index = dims.iter().position(|&dim| dim.is_err()).unwrap();
                let (front, back) = dims.split_at(inf_index);
                let rev = back[0].unwrap_err();
                let rev_mul = if rev { -1 } else { 1 };
                let back = &back[1..];
                let front_len = front.iter().flatten().product::<isize>().unsigned_abs();
                let back_len = back.iter().flatten().product::<isize>().unsigned_abs();
                if front_len == 0 || back_len == 0 {
                    return Err(env.error("Cannot reshape array with any 0 outer dimensions"));
                }
                let middle_len = rev_mul * derive_len(shape.iter().product(), front_len * back_len);
                let mut axes: Vec<isize> = front.iter().copied().flatten().collect();
                axes.push(middle_len);
                axes.extend(back.iter().flatten());
                axes
            }
        }
        n => return Err(env.error(format!("Cannot reshape array with {n} infinite dimensions"))),
    })
}

impl Value {
    /// `rerank` this value with another
    pub fn rerank(&mut self, rank: &Self, env: &Uiua) -> UiuaResult {
        self.take_map_keys();
        let irank = rank.as_int(env, "Rank must be an integer")?;
        let shape = self.shape_mut();
        let rank = irank.unsigned_abs();
        if irank >= 0 {
            // Positive rank
            if rank >= shape.len() {
                for _ in 0..rank - shape.len() + 1 {
                    shape.insert(0, 1);
                }
            } else {
                let mid = shape.len() - rank;
                let new_first_dim: usize = shape[..mid].iter().product();
                *shape = once(new_first_dim)
                    .chain(shape[mid..].iter().copied())
                    .collect();
            }
        } else {
            // Negative rank
            if rank > shape.len() {
                return Err(env.error(format!(
                    "Negative rerank has magnitude {}, which is greater \
                    than the array's rank {}",
                    rank,
                    shape.len()
                )));
            }
            let new_first_dim: usize = shape[..rank].iter().product();
            *shape = once(new_first_dim)
                .chain(shape[rank..].iter().copied())
                .collect();
        }
        self.validate_shape();
        Ok(())
    }
    pub(crate) fn undo_rerank(&mut self, rank: &Self, orig_shape: &Self, env: &Uiua) -> UiuaResult {
        if self.rank() == 0 {
            if let Value::Box(arr) = self {
                arr.data.as_mut_slice()[0]
                    .0
                    .undo_rerank(rank, orig_shape, env)?;
            }
            return Ok(());
        }
        let irank = rank.as_int(env, "Rank must be an integer")?;
        let orig_shape = orig_shape.as_nats(env, "Shape must be a list of natural numbers")?;
        let rank = irank.unsigned_abs();
        let new_shape: Shape = if irank >= 0 {
            // Positive rank
            orig_shape
                .iter()
                .take(orig_shape.len().saturating_sub(rank))
                .chain(
                    (self.shape().iter()).skip((rank + 1).saturating_sub(orig_shape.len()).max(1)),
                )
                .copied()
                .collect()
        } else {
            // Negative rank
            (orig_shape.iter().take(rank))
                .chain(self.shape().iter().skip(1))
                .copied()
                .collect()
        };
        if new_shape.iter().product::<usize>() != self.element_count() {
            return Ok(());
        }
        *self.shape_mut() = new_shape;
        self.validate_shape();
        Ok(())
    }
}

impl Value {
    /// Use this value as counts to `keep` another
    pub fn keep(&self, kept: Self, env: &Uiua) -> UiuaResult<Self> {
        let counts = self.as_nats(
            env,
            "Keep amount must be a natural number \
            or list of natural numbers",
        )?;
        Ok(if self.rank() == 0 {
            match kept {
                Value::Num(a) => a.scalar_keep(counts[0]).into(),
                Value::Byte(a) => a.scalar_keep(counts[0]).into(),
                Value::Complex(a) => a.scalar_keep(counts[0]).into(),
                Value::Char(a) => a.scalar_keep(counts[0]).into(),
                Value::Box(a) => a.scalar_keep(counts[0]).into(),
            }
        } else {
            match kept {
                Value::Num(a) => a.list_keep(&counts, env)?.into(),
                Value::Byte(a) => a.list_keep(&counts, env)?.into(),
                Value::Complex(a) => a.list_keep(&counts, env)?.into(),
                Value::Char(a) => a.list_keep(&counts, env)?.into(),
                Value::Box(a) => a.list_keep(&counts, env)?.into(),
            }
        })
    }
    pub(crate) fn undo_keep(self, kept: Self, into: Self, env: &Uiua) -> UiuaResult<Self> {
        let counts = self.as_nats(
            env,
            "Keep amount must be a natural number \
            or list of natural numbers",
        )?;
        if self.rank() == 0 {
            return Err(env.error("Cannot invert scalar keep"));
        }
        kept.generic_bin_into(
            into,
            |a, b| a.undo_keep(&counts, b, env).map(Into::into),
            |a, b| a.undo_keep(&counts, b, env).map(Into::into),
            |a, b| a.undo_keep(&counts, b, env).map(Into::into),
            |a, b| a.undo_keep(&counts, b, env).map(Into::into),
            |a, b| a.undo_keep(&counts, b, env).map(Into::into),
            |a, b| env.error(format!("Cannot unkeep {a} array with {b} array")),
        )
    }
}

impl<T: ArrayValue> Array<T> {
    /// `keep` this array by replicating it as the rows of a new array
    pub fn scalar_keep(mut self, count: usize) -> Self {
        // Scalar kept
        if self.rank() == 0 {
            self.shape.push(count);
            let value = self.data[0].clone();
            self.data.clear();
            unsafe {
                self.data
                    .extend_from_trusted((0..count).map(|_| value.clone()))
            };
            self.validate_shape();
            return self;
        }
        // Keep nothing
        if count == 0 {
            self.data = CowSlice::new();
            self.shape[0] = 0;
            return self;
        }
        // Keep 1 is a no-op
        if count == 1 {
            return self;
        }
        // Keep ≥2 is a repeat
        let mut new_data = EcoVec::with_capacity(count * self.data.len());
        for row in self.row_slices() {
            for _ in 0..count {
                new_data.extend_from_slice(row);
            }
        }
        self.shape[0] *= count;
        self.data = new_data.into();
        self.validate_shape();
        self
    }
    /// `keep` this array with some counts
    pub fn list_keep(mut self, counts: &[usize], env: &Uiua) -> UiuaResult<Self> {
        if counts.len() > self.row_count() {
            return Err(env.error(format!(
                "Cannot keep array with shape {} with array of length {}",
                self.shape(),
                counts.len()
            )));
        }
        self.take_map_keys();
        let fill = env
            .num_array_fill()
            .map_err(|e| {
                env.error(format!(
                    "Cannot keep array with shape {} with array of shape {}{e}",
                    self.shape(),
                    FormatShape(&[counts.len()])
                ))
            })
            .and_then(|fill| {
                if let Some(n) = fill.data.iter().find(|&&n| n < 0.0 || n.fract() != 0.0) {
                    return Err(env.error(format!(
                        "Fill value for keep must be an array of \
                        non-negative integers, but one of the \
                        values is {n}"
                    )));
                }
                Ok(fill)
            });
        let fill = if counts.len() < self.row_count() {
            Some(fill?)
        } else {
            None
        };
        let mut all_boolean = true;
        let sum: usize = counts
            .iter()
            .copied()
            .chain(
                (fill.as_ref().into_iter())
                    .flat_map(|arr| arr.data.iter().map(|&n| n as usize))
                    .cycle(),
            )
            .take(self.row_count())
            .inspect(|&n| all_boolean &= n <= 1)
            .sum();
        let get_count = |i: usize| {
            counts.get(i).copied().unwrap_or_else(|| {
                let fill = fill.as_ref().unwrap();
                fill.data[(i - counts.len()) % fill.row_count()] as usize
            })
        };
        let row_len = self.row_len();
        let row_count = self.row_count();
        if all_boolean {
            let mut elem_idx = 0;
            self.data.retain(|_| {
                let keep = get_count(elem_idx / row_len) == 1;
                elem_idx += 1;
                keep
            });
        } else {
            'efficient: {
                let data = self.data.as_mut_slice();
                let mut src = 0;
                let mut dest = 0;
                for r in 0..row_count {
                    let count = get_count(r);
                    // println!("r: {r}, count: {count}, src: {src}, dest: {dest}");
                    if count == 0 {
                        src += 1
                    } else {
                        if dest + count > src + 1 {
                            let old_data = self.data.clone();
                            self.data.truncate(dest * row_len);
                            for r in r..row_count {
                                let count = get_count(r);
                                let start = src * row_len;
                                for _ in 0..count {
                                    self.data
                                        .extend_from_slice(&old_data[start..start + row_len]);
                                }
                                src += 1;
                            }
                            break 'efficient;
                        }
                        let count_start = if src == dest { 1 } else { 0 };
                        for c in count_start..count {
                            // println!("  c: {c}");
                            for j in 0..row_len {
                                let dest = (dest + c) * row_len + j;
                                let src = src * row_len + j;
                                // println!("    j: {j}, {src} -> {dest}");
                                data[dest] = data[src].clone();
                            }
                        }
                        src += 1;
                        dest += count;
                    }
                }
            }
            self.data.truncate(sum * row_len);
        }
        self.shape[0] = sum;
        self.validate_shape();
        Ok(self)
    }
    fn undo_keep(self, counts: &[usize], into: Self, env: &Uiua) -> UiuaResult<Self> {
        if counts.iter().any(|&n| n > 1) {
            return Err(env.error("Cannot invert keep with non-boolean counts"));
        }
        let mut new_rows: Vec<_> = Vec::with_capacity(counts.len());
        let mut transformed = self.into_rows();
        for (count, into_row) in counts.iter().zip(into.into_rows()) {
            if *count == 0 {
                new_rows.push(into_row);
            } else {
                let new_row = transformed.next().ok_or_else(|| {
                    env.error(
                        "Kept array has fewer rows than it was created with, \
                        so the keep cannot be inverted",
                    )
                })?;
                if new_row.shape != into_row.shape {
                    return Err(env.error(format!(
                        "Kept array's shape was changed from {} to {}, \
                        so the keep cannot be inverted",
                        into_row.shape(),
                        new_row.shape()
                    )));
                }
                new_rows.push(new_row);
            }
        }
        Self::from_row_arrays(new_rows, env)
    }
}

impl Value {
    /// Use this value to `rotate` another
    pub fn rotate(&self, rotated: Self, env: &Uiua) -> UiuaResult<Self> {
        self.rotate_depth(rotated, 0, 0, env)
    }
    pub(crate) fn rotate_depth(
        &self,
        mut rotated: Self,
        a_depth: usize,
        b_depth: usize,
        env: &Uiua,
    ) -> UiuaResult<Self> {
        if self.row_count() == 0 {
            return Ok(rotated);
        }
        let by_ints = || self.as_integer_array(env, "Rotation amount must be an array of integers");
        if env.num_scalar_fill().is_ok() {
            if let Value::Byte(bytes) = &rotated {
                rotated = bytes.convert_ref::<f64>().into();
            }
        }
        match &mut rotated {
            Value::Num(a) => a.rotate_depth(by_ints()?, b_depth, a_depth, env)?,
            Value::Byte(a) => a.rotate_depth(by_ints()?, b_depth, a_depth, env)?,
            Value::Complex(a) => a.rotate_depth(by_ints()?, b_depth, a_depth, env)?,
            Value::Char(a) => a.rotate_depth(by_ints()?, b_depth, a_depth, env)?,
            Value::Box(a) if a.rank() == a_depth => {
                for Boxed(val) in a.data.as_mut_slice() {
                    *val = self.rotate_depth(take(val), a_depth, b_depth, env)?;
                }
            }
            Value::Box(a) => a.rotate_depth(by_ints()?, b_depth, a_depth, env)?,
        }
        Ok(rotated)
    }
}

impl<T: ArrayValue> Array<T> {
    /// `rotate` this array by the given amount
    pub fn rotate(&mut self, by: Array<isize>, env: &Uiua) -> UiuaResult {
        self.rotate_depth(by, 0, 0, env)
    }
    pub(crate) fn rotate_depth(
        &mut self,
        by: Array<isize>,
        depth: usize,
        by_depth: usize,
        env: &Uiua,
    ) -> UiuaResult {
        let mut filled = false;
        let fill = env.scalar_fill::<T>();
        self.depth_slices(&by, depth, by_depth, env, |ash, a, bsh, b, env| {
            if bsh.len() > 1 {
                return Err(env.error(format!("Cannot rotate by rank {} array", bsh.len())));
            }
            if b.len() > ash.len() {
                return Err(env.error(format!(
                    "Cannot rotate rank {} array with index of length {}",
                    ash.len(),
                    b.len()
                )));
            }
            rotate(b, ash, a);
            if let Ok(fill) = &fill {
                fill_shift(b, ash, a, fill.clone());
                filled = true;
            }
            Ok(())
        })?;
        if filled {
            self.reset_meta_flags();
        }
        if depth == 0 {
            if let Some(keys) = self.map_keys_mut() {
                let by = by.data[0];
                keys.rotate(by);
            }
        }
        Ok(())
    }
}

fn rotate<T>(by: &[isize], shape: &[usize], data: &mut [T]) {
    if by.is_empty() || shape.is_empty() {
        return;
    }
    let row_count = shape[0];
    if row_count == 0 {
        return;
    }
    let row_len = shape[1..].iter().product();
    let offset = by[0];
    let mid = (row_count as isize + offset).rem_euclid(row_count as isize) as usize;
    let (left, right) = data.split_at_mut(mid * row_len);
    left.reverse();
    right.reverse();
    data.reverse();
    let index = &by[1..];
    let shape = &shape[1..];
    if index.is_empty() || shape.is_empty() {
        return;
    }
    for cell in data.chunks_mut(row_len) {
        rotate(index, shape, cell);
    }
}

fn fill_shift<T: Clone>(by: &[isize], shape: &[usize], data: &mut [T], fill: T) {
    if by.is_empty() || shape.is_empty() {
        return;
    }
    let row_count = shape[0];
    if row_count == 0 {
        return;
    }
    let offset = by[0];
    let row_len: usize = shape[1..].iter().product();
    if offset != 0 {
        let abs_offset = offset.unsigned_abs() * row_len;
        let data_len = data.len();
        if offset > 0 {
            for val in &mut data[data_len.saturating_sub(abs_offset)..] {
                *val = fill.clone();
            }
        } else {
            for val in &mut data[..abs_offset.min(data_len)] {
                *val = fill.clone();
            }
        }
    }
    let index = &by[1..];
    let shape = &shape[1..];
    if index.is_empty() || shape.is_empty() {
        return;
    }
    for cell in data.chunks_mut(row_len) {
        fill_shift(index, shape, cell, fill.clone());
    }
}

impl Value {
    /// Use this array to `windows` another
    pub fn windows(&self, from: &Self, env: &Uiua) -> UiuaResult<Self> {
        let size_spec = self.as_ints(env, "Window size must be an integer or list of integers")?;
        Ok(match from {
            Value::Num(a) => a.windows(&size_spec, env)?.into(),
            Value::Byte(a) => a.windows(&size_spec, env)?.into(),
            Value::Complex(a) => a.windows(&size_spec, env)?.into(),
            Value::Char(a) => a.windows(&size_spec, env)?.into(),
            Value::Box(a) => a.windows(&size_spec, env)?.into(),
        })
    }
}

impl<T: ArrayValue> Array<T> {
    /// Get the `windows` of this array
    pub fn windows(&self, isize_spec: &[isize], env: &Uiua) -> UiuaResult<Self> {
        if isize_spec.iter().any(|&s| s == 0) {
            return Err(env.error("Window size cannot be zero"));
        }
        if isize_spec.len() > self.shape.len() {
            return Err(env.error(format!(
                "Window size {isize_spec:?} has too many axes for shape {}",
                self.shape()
            )));
        }
        let mut size_spec = Vec::with_capacity(isize_spec.len());
        for (d, s) in self.shape.iter().zip(isize_spec) {
            size_spec.push(if *s >= 0 { *s } else { *d as isize + 1 + *s });
        }
        // Determine the shape of the windows array
        let mut new_shape = Shape::with_capacity(self.shape.len() + size_spec.len());
        new_shape.extend(
            self.shape
                .iter()
                .zip(&size_spec)
                .map(|(a, b)| ((*a as isize + 1) - *b).max(0) as usize),
        );
        new_shape.extend(size_spec.iter().map(|&s| s.max(0) as usize));
        new_shape.extend_from_slice(&self.shape[size_spec.len()..]);
        // Check if the window size is too large
        for (size, sh) in size_spec.iter().zip(&self.shape) {
            if *size <= 0 || *size > *sh as isize {
                return Ok(Self::new(new_shape, CowSlice::new()));
            }
        }
        // Make a new window shape with the same rank as the windowed array
        let mut true_size: Vec<usize> = Vec::with_capacity(self.shape.len());
        true_size.extend(size_spec.iter().map(|&s| s as usize));
        if true_size.len() < self.shape.len() {
            true_size.extend(&self.shape[true_size.len()..]);
        }

        let mut dst = EcoVec::from_elem(self.data[0].clone(), new_shape.iter().product());
        let dst_slice = dst.make_mut();
        let mut corner = vec![0; self.shape.len()];
        let mut curr = vec![0; self.shape.len()];
        let mut k = 0;
        'windows: loop {
            // Reset curr
            for i in curr.iter_mut() {
                *i = 0;
            }
            // Copy the window at the current corner
            'items: loop {
                // Copy the current item
                let mut src_index = 0;
                let mut stride = 1;
                for ((c, i), s) in corner.iter().zip(&curr).zip(&self.shape).rev() {
                    src_index += (*c + *i) * stride;
                    stride *= s;
                }
                dst_slice[k] = self.data[src_index].clone();
                k += 1;
                // Go to the next item
                for i in (0..curr.len()).rev() {
                    if curr[i] == true_size[i] - 1 {
                        curr[i] = 0;
                    } else {
                        curr[i] += 1;
                        continue 'items;
                    }
                }
                break;
            }
            // Go to the next corner
            for i in (0..corner.len()).rev() {
                if corner[i] == self.shape[i] - true_size[i] {
                    corner[i] = 0;
                } else {
                    corner[i] += 1;
                    continue 'windows;
                }
            }
            break Ok(Array::new(new_shape, dst));
        }
    }
}

impl Value {
    /// Try to `find` this value in another
    pub fn find(&self, searched: &Self, env: &Uiua) -> UiuaResult<Self> {
        self.generic_bin_ref(
            searched,
            |a, b| a.find(b, env).map(Into::into),
            |a, b| a.find(b, env).map(Into::into),
            |a, b| a.find(b, env).map(Into::into),
            |a, b| a.find(b, env).map(Into::into),
            |a, b| a.find(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot find {} in {} array",
                    a.type_name(),
                    b.type_name()
                ))
            },
        )
    }
    /// Try to `mask` this value in another
    pub fn mask(&self, searched: &Self, env: &Uiua) -> UiuaResult<Self> {
        self.generic_bin_ref(
            searched,
            |a, b| a.mask(b, env).map(Into::into),
            |a, b| a.mask(b, env).map(Into::into),
            |a, b| a.mask(b, env).map(Into::into),
            |a, b| a.mask(b, env).map(Into::into),
            |a, b| a.mask(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot mask {} in {} array",
                    a.type_name(),
                    b.type_name()
                ))
            },
        )
    }
}

impl<T: ArrayValue> Array<T> {
    /// Try to `find` this array in another
    pub fn find(&self, searched: &Self, env: &Uiua) -> UiuaResult<Array<u8>> {
        let searched_for = self;
        let mut searched = searched;
        let mut local_searched: Self;
        let any_dim_greater = (searched_for.shape().iter().rev())
            .zip(searched.shape().iter().rev())
            .any(|(a, b)| a > b);
        if self.rank() > searched.rank() || any_dim_greater {
            // Fill
            match env.scalar_fill() {
                Ok(fill) => {
                    let mut target_shape = searched.shape.clone();
                    target_shape[0] = searched_for.row_count();
                    local_searched = searched.clone();
                    local_searched.fill_to_shape(&target_shape, fill);
                    searched = &local_searched;
                }
                Err(_) => {
                    let data = cowslice![0; searched.element_count()];
                    let mut arr = Array::new(searched.shape.clone(), data);
                    arr.meta_mut().flags.set(ArrayFlags::BOOLEAN, true);
                    return Ok(arr);
                }
            }
        }

        // Pad the shape of the searched-for array
        let mut searched_for_shape = searched_for.shape.clone();
        while searched_for_shape.len() < searched.shape.len() {
            searched_for_shape.insert(0, 1);
        }

        // Calculate the pre-padded output shape
        let temp_output_shape: Shape = searched
            .shape
            .iter()
            .zip(&searched_for_shape)
            .map(|(s, f)| s + 1 - f)
            .collect();

        let mut data = EcoVec::from_elem(0, temp_output_shape.iter().product());
        let data_slice = data.make_mut();
        let mut corner = vec![0; searched.shape.len()];
        let mut curr = vec![0; searched.shape.len()];
        let mut k = 0;

        if searched.shape.iter().all(|&d| d > 0) {
            'windows: loop {
                // Reset curr
                for i in curr.iter_mut() {
                    *i = 0;
                }
                // Search the window whose top-left is the current corner
                'items: loop {
                    // Get index for the current item in the searched array
                    let mut searched_index = 0;
                    let mut stride = 1;
                    for ((c, i), s) in corner.iter().zip(&curr).zip(&searched.shape).rev() {
                        searched_index += (*c + *i) * stride;
                        stride *= s;
                    }
                    // Get index for the current item in the searched-for array
                    let mut search_for_index = 0;
                    let mut stride = 1;
                    for (i, s) in curr.iter().zip(&searched_for_shape).rev() {
                        search_for_index += *i * stride;
                        stride *= s;
                    }
                    // Compare the current items in the two arrays
                    let same = if let Some(searched_for) = searched_for.data.get(search_for_index) {
                        searched.data[searched_index].array_eq(searched_for)
                    } else {
                        false
                    };
                    if !same {
                        data_slice[k] = 0;
                        k += 1;
                        break;
                    }
                    // Go to the next item
                    for i in (0..curr.len()).rev() {
                        if curr[i] == searched_for_shape[i] - 1 {
                            curr[i] = 0;
                        } else {
                            curr[i] += 1;
                            continue 'items;
                        }
                    }
                    data_slice[k] = 1;
                    k += 1;
                    break;
                }
                // Go to the next corner
                for i in (0..corner.len()).rev() {
                    if corner[i] == searched.shape[i] - searched_for_shape[i] {
                        corner[i] = 0;
                    } else {
                        corner[i] += 1;
                        continue 'windows;
                    }
                }
                break;
            }
        }
        let mut arr = Array::new(temp_output_shape, data);
        arr.fill_to_shape(&searched.shape[..searched_for_shape.len()], 0);
        arr.validate_shape();
        arr.meta_mut().flags.set(ArrayFlags::BOOLEAN, true);
        Ok(arr)
    }
    /// Try to `mask` this array in another
    pub fn mask(&self, haystack: &Self, env: &Uiua) -> UiuaResult<Value> {
        let needle = self;
        if needle.rank() > haystack.rank() {
            return Err(env.error(format!(
                "Cannot look for rank {} array in rank {} array",
                needle.rank(),
                haystack.rank()
            )));
        }
        if (needle.shape.iter().rev())
            .zip(haystack.shape.iter().rev())
            .any(|(n, h)| n > h)
        {
            return Ok(Array::new(
                haystack.shape.clone(),
                eco_vec![0u8; haystack.element_count()],
            )
            .into());
        }
        let mut result_data = eco_vec![0.0; haystack.element_count()];
        let res = result_data.make_mut();
        let needle_data = needle.data.as_slice();
        let mut needle_shape = needle.shape.clone();
        while needle_shape.len() < haystack.shape.len() {
            needle_shape.insert(0, 1);
        }
        let needle_elems = needle.element_count();
        let mut curr = Vec::new();
        let mut offset = Vec::new();
        let mut sum = vec![0; needle_shape.len()];
        let mut match_num = 0u64;
        for i in 0..res.len() {
            // Check if the needle matches the haystack at the current index
            haystack.shape.flat_to_dims(i, &mut curr);
            let mut matches = true;
            for j in 0..needle_elems {
                needle_shape.flat_to_dims(j, &mut offset);
                for ((c, o), s) in curr.iter().zip(&offset).zip(&mut sum) {
                    *s = *c + *o;
                }
                if (haystack.shape.dims_to_flat(&sum)).map_or(true, |k| {
                    res[k] > 0.0 || !needle_data[j].array_eq(&haystack.data[k])
                }) {
                    matches = false;
                    break;
                }
            }
            // Fill matches
            if matches {
                match_num += 1;
                for j in 0..needle_elems {
                    needle_shape.flat_to_dims(j, &mut offset);
                    for ((c, o), s) in curr.iter().zip(&offset).zip(&mut sum) {
                        *s = *c + *o;
                    }
                    let k = haystack.shape.dims_to_flat(&sum).unwrap();
                    res[k] = match_num as f64;
                }
            }
        }
        let mut val: Value = Array::new(haystack.shape.clone(), result_data).into();
        val.compress();
        Ok(val)
    }
}

impl Value {
    /// Check which rows of this value are `member`s of another
    pub fn member(&self, of: &Self, env: &Uiua) -> UiuaResult<Self> {
        self.generic_bin_ref(
            of,
            |a, b| a.member(b, env).map(Into::into),
            |a, b| a.member(b, env).map(Into::into),
            |a, b| a.member(b, env).map(Into::into),
            |a, b| a.member(b, env).map(Into::into),
            |a, b| a.member(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot look for members of {} array in {} array",
                    a.type_name(),
                    b.type_name(),
                ))
            },
        )
    }
}

impl<T: ArrayValue> Array<T> {
    /// Check which rows of this array are `member`s of another
    pub fn member(&self, of: &Self, env: &Uiua) -> UiuaResult<Array<u8>> {
        let elems = self;
        let mut arr = match elems.rank().cmp(&of.rank()) {
            Ordering::Equal => {
                let mut result_data = EcoVec::with_capacity(elems.row_count());
                let mut members = HashSet::with_capacity(of.row_count());
                for of in of.row_slices() {
                    members.insert(ArrayCmpSlice(of));
                }
                for elem in elems.row_slices() {
                    result_data.push(members.contains(&ArrayCmpSlice(elem)) as u8);
                }
                let shape: Shape = self.shape.iter().cloned().take(1).collect();
                Array::new(shape, result_data)
            }
            Ordering::Greater => {
                let mut rows = Vec::with_capacity(elems.row_count());
                for elem in elems.rows() {
                    rows.push(elem.member(of, env)?);
                }
                Array::from_row_arrays(rows, env)?
            }
            Ordering::Less => {
                if !of.shape.ends_with(&elems.shape) {
                    return Err(env.error(format!(
                        "Cannot look for array of shape {} in array of shape {}",
                        self.shape, of.shape
                    )));
                }
                if of.rank() - elems.rank() == 1 {
                    of.rows().any(|r| *elems == r).into()
                } else {
                    let mut rows = Vec::with_capacity(of.row_count());
                    for of in of.rows() {
                        rows.push(elems.member(&of, env)?);
                    }
                    Array::from_row_arrays(rows, env)?
                }
            }
        };
        arr.meta_mut().flags.set(ArrayFlags::BOOLEAN, true);
        Ok(arr)
    }
}

impl Value {
    /// Get the `index of` the rows of this value in another
    pub fn index_of(&self, haystack: &Value, env: &Uiua) -> UiuaResult<Value> {
        self.generic_bin_ref(
            haystack,
            |a, b| a.index_of(b, env).map(Into::into),
            |a, b| a.index_of(b, env).map(Into::into),
            |a, b| a.index_of(b, env).map(Into::into),
            |a, b| a.index_of(b, env).map(Into::into),
            |a, b| a.index_of(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot look for indices of {} array in {} array",
                    a.type_name(),
                    b.type_name(),
                ))
            },
        )
    }
    /// Get the `coordinate` of the rows of this value in another
    pub fn coordinate(&self, haystack: &Value, env: &Uiua) -> UiuaResult<Value> {
        self.generic_bin_ref(
            haystack,
            |a, b| a.coordinate(b, env).map(Into::into),
            |a, b| a.coordinate(b, env).map(Into::into),
            |a, b| a.coordinate(b, env).map(Into::into),
            |a, b| a.coordinate(b, env).map(Into::into),
            |a, b| a.coordinate(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot look for coordinates of {} array in {} array",
                    a.type_name(),
                    b.type_name(),
                ))
            },
        )
    }
    /// Get the `progressive index of` the rows of this value in another
    pub fn progressive_index_of(&self, searched_in: &Value, env: &Uiua) -> UiuaResult<Value> {
        self.generic_bin_ref(
            searched_in,
            |a, b| a.progressive_index_of(b, env).map(Into::into),
            |a, b| a.progressive_index_of(b, env).map(Into::into),
            |a, b| a.progressive_index_of(b, env).map(Into::into),
            |a, b| a.progressive_index_of(b, env).map(Into::into),
            |a, b| a.progressive_index_of(b, env).map(Into::into),
            |a, b| {
                env.error(format!(
                    "Cannot look for indices of {} array in {} array",
                    a.type_name(),
                    b.type_name(),
                ))
            },
        )
    }
}

impl<T: ArrayValue> Array<T> {
    /// Get the `index of` the rows of this array in another
    pub fn index_of(&self, haystack: &Array<T>, env: &Uiua) -> UiuaResult<Array<f64>> {
        let needle = self;
        Ok(match needle.rank().cmp(&haystack.rank()) {
            Ordering::Equal => {
                let mut result_data = EcoVec::with_capacity(needle.row_count());
                let mut members = HashMap::with_capacity(haystack.row_count());
                for (i, of) in haystack.row_slices().enumerate() {
                    members.entry(ArrayCmpSlice(of)).or_insert(i);
                }
                for elem in needle.row_slices() {
                    result_data.push(
                        members
                            .get(&ArrayCmpSlice(elem))
                            .map(|i| *i as f64)
                            .unwrap_or(haystack.row_count() as f64),
                    );
                }
                let shape: Shape = self.shape.iter().cloned().take(1).collect();
                Array::new(shape, result_data)
            }
            Ordering::Greater => {
                let mut rows = Vec::with_capacity(needle.row_count());
                for elem in needle.rows() {
                    rows.push(elem.index_of(haystack, env)?);
                }
                Array::from_row_arrays(rows, env)?
            }
            Ordering::Less => {
                if !haystack.shape.ends_with(&needle.shape) {
                    return Err(env.error(format!(
                        "Cannot get index of array of shape {} in array of shape {}",
                        needle.shape(),
                        haystack.shape()
                    )));
                }
                if haystack.rank() - needle.rank() == 1 {
                    (haystack
                        .row_slices()
                        .position(|r| {
                            r.len() == needle.data.len()
                                && r.iter().zip(&needle.data).all(|(a, b)| a.array_eq(b))
                        })
                        .unwrap_or(haystack.row_count()) as f64)
                        .into()
                } else {
                    let mut rows = Vec::with_capacity(haystack.row_count());
                    for of in haystack.rows() {
                        rows.push(needle.index_of(&of, env)?);
                    }
                    Array::from_row_arrays(rows, env)?
                }
            }
        })
    }
    /// Get the `coordinate` of the rows of this array in another
    pub fn coordinate(&self, haystack: &Array<T>, env: &Uiua) -> UiuaResult<Array<f64>> {
        let needle = self;
        Ok(match needle.rank().cmp(&haystack.rank()) {
            Ordering::Equal => {
                let mut result_data = EcoVec::with_capacity(needle.row_count());
                let mut members = HashMap::with_capacity(haystack.row_count());
                for (i, of) in haystack.row_slices().enumerate() {
                    members.entry(ArrayCmpSlice(of)).or_insert(i);
                }
                for elem in needle.row_slices() {
                    result_data.push(
                        members
                            .get(&ArrayCmpSlice(elem))
                            .map(|i| *i as f64)
                            .unwrap_or(haystack.row_count() as f64),
                    );
                }
                let mut shape: Shape = self.shape.iter().cloned().take(1).collect();
                shape.push(1);
                Array::new(shape, result_data)
            }
            Ordering::Greater => {
                let mut rows = Vec::with_capacity(needle.row_count());
                for elem in needle.rows() {
                    rows.push(elem.coordinate(haystack, env)?);
                }
                Array::from_row_arrays(rows, env)?
            }
            Ordering::Less => {
                if !haystack.shape.ends_with(&needle.shape) {
                    return Err(env.error(format!(
                        "Cannot get coordinate of array of shape {} \
                        in array of shape {}",
                        needle.shape(),
                        haystack.shape()
                    )));
                }
                let haystack_item_len: usize =
                    haystack.shape.iter().rev().take(needle.rank()).product();
                if haystack_item_len == 0 {
                    todo!()
                }
                let outer_hay_shape =
                    Shape::from(&haystack.shape[..haystack.rank() - needle.rank()]);
                let index = if let Some(raw_index) = (haystack.data.chunks_exact(haystack_item_len))
                    .position(|ch| ch.iter().zip(&needle.data).all(|(a, b)| a.array_eq(b)))
                {
                    let mut index = Vec::new();
                    outer_hay_shape.flat_to_dims(raw_index, &mut index);
                    index
                } else {
                    outer_hay_shape.to_vec()
                };
                if index.len() == 1 {
                    (index[0] as f64).into()
                } else {
                    index.into()
                }
            }
        })
    }
    /// Get the `progressive index of` the rows of this array in another
    fn progressive_index_of(&self, searched_in: &Array<T>, env: &Uiua) -> UiuaResult<Array<f64>> {
        let searched_for = self;
        Ok(match searched_for.rank().cmp(&searched_in.rank()) {
            Ordering::Equal => {
                let mut used = HashSet::new();
                let mut result_data = EcoVec::with_capacity(searched_for.row_count());
                if searched_for.rank() == 1 {
                    for elem in &searched_for.data {
                        let mut hasher = DefaultHasher::new();
                        elem.array_hash(&mut hasher);
                        let hash = hasher.finish();
                        result_data.push(
                            (searched_in.data.iter().enumerate())
                                .find(|&(i, of)| elem.array_eq(of) && used.insert((hash, i)))
                                .map(|(i, _)| i)
                                .unwrap_or(searched_in.row_count())
                                as f64,
                        );
                    }
                    return Ok(Array::from(result_data));
                }
                'elem: for elem in searched_for.rows() {
                    for (i, of) in searched_in.rows().enumerate() {
                        let mut hasher = DefaultHasher::new();
                        elem.hash(&mut hasher);
                        let hash = hasher.finish();
                        if elem == of && used.insert((hash, i)) {
                            result_data.push(i as f64);
                            continue 'elem;
                        }
                    }
                    result_data.push(searched_in.row_count() as f64);
                }
                let shape: Shape = self.shape.iter().cloned().take(1).collect();
                Array::new(shape, result_data)
            }
            Ordering::Greater => {
                let mut rows = Vec::with_capacity(searched_for.row_count());
                for elem in searched_for.rows() {
                    rows.push(elem.progressive_index_of(searched_in, env)?);
                }
                Array::from_row_arrays(rows, env)?
            }
            Ordering::Less => {
                if searched_in.rank() - searched_for.rank() == 1 {
                    if searched_for.rank() == 0 {
                        let searched_for = &searched_for.data[0];
                        Array::from(
                            (searched_in.data.iter())
                                .position(|of| searched_for.array_eq(of))
                                .unwrap_or(searched_in.row_count())
                                as f64,
                        )
                    } else {
                        ((searched_in.rows().position(|r| r == *searched_for))
                            .unwrap_or(searched_in.row_count()) as f64)
                            .into()
                    }
                } else {
                    let mut rows = Vec::with_capacity(searched_in.row_count());
                    for of in searched_in.rows() {
                        rows.push(searched_for.progressive_index_of(&of, env)?);
                    }
                    Array::from_row_arrays(rows, env)?
                }
            }
        })
    }
}

impl Array<f64> {
    pub(crate) fn matrix_mul(&self, other: &Self, env: &Uiua) -> UiuaResult<Self> {
        let (a, b) = (self, other);
        let a_row_shape = a.shape().row();
        let b_row_shape = b.shape().row();
        if !shape_prefixes_match(&a_row_shape, &b_row_shape) {
            return Err(env.error(format!(
                "Cannot multiply arrays of shape {} and {}",
                a.shape(),
                b.shape()
            )));
        }
        let prod_shape = if a_row_shape.len() >= b_row_shape.len() {
            &a_row_shape
        } else {
            &b_row_shape
        };
        let prod_row_shape = prod_shape.row();
        let prod_elems = prod_row_shape.elements();
        let mut result_data = eco_vec![0.0; self.row_count() * other.row_count() * prod_elems];
        let result_slice = result_data.make_mut();
        let mut result_shape = Shape::from([a.row_count(), b.row_count()]);
        result_shape.extend(prod_row_shape.iter().copied());
        let inner = |a_row: &[f64], res_row: &mut [f64]| {
            let mut prod_row = vec![0.0; prod_shape.elements()];
            let mut i = 0;
            for b_row in b.row_slices() {
                _ = bin_pervade_recursive(
                    &(&*a_row_shape, a_row),
                    &(&*b_row_shape, b_row),
                    &mut prod_row,
                    env,
                    InfalliblePervasiveFn::new(pervade::mul::num_num),
                );
                let (sum, rest) = prod_row.split_at_mut(prod_elems);
                for chunk in rest.chunks_exact(prod_elems) {
                    for (a, b) in sum.iter_mut().zip(chunk.iter()) {
                        *a += *b;
                    }
                }
                res_row[i..i + prod_elems].copy_from_slice(sum);
                i += prod_elems;
            }
        };
        let iter = (a.row_slices()).zip(result_slice.chunks_exact_mut(b.row_count() * prod_elems));
        if a.row_count() > 100 || b.row_count() > 100 {
            (iter.par_bridge()).for_each(|(a_row, res_row)| inner(a_row, res_row));
        } else {
            iter.for_each(|(a_row, res_row)| inner(a_row, res_row));
        }
        Ok(Array::new(result_shape, result_data))
    }
}
