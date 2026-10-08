/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Ported from ICU, under the Unicode License v3 (LICENSE-UNICODE). ICU's notice:
// © 2016 and later: Unicode, Inc. and others.
// License & terms of use: http://www.unicode.org/copyright.html
// Copyright (C) 2007-2016, International Business Machines Corporation and
// others. All Rights Reserved.

//! A port of ICU 74's `DateTimePatternGenerator` (`i18n/dtptngen.cpp`, the version the OS's ICU
//! runs) over the CLDR data of `patterns.rs`: the NDK does not export `udatpg_*`.
//!
//! Only `getBestPattern` with `UDATPG_MATCH_HOUR_FIELD_LENGTH` (what ECMA-402 engines use) is
//! ported, for skeletons without the `j`/`J`/`C` metacharacters.

const FIELD_COUNT: usize = 16;
const MONTH: usize = 3;
const WEEKDAY: usize = 6;
const DAYPERIOD: usize = 10;
const HOUR: usize = 11;
const MINUTE: usize = 12;
const SECOND: usize = 13;
const FRACTIONAL_SECOND: usize = 14;
const YEAR: usize = 1;

const DATE_MASK: u32 = (1 << DAYPERIOD) - 1;
const TIME_MASK: u32 = (1 << FIELD_COUNT) - 1 - DATE_MASK;
const FRACTIONAL_MASK: u32 = 1 << FRACTIONAL_SECOND;
const SECOND_AND_FRACTIONAL_MASK: u32 = (1 << SECOND) | FRACTIONAL_MASK;

const NUMERIC: i32 = 0x100;
const DELTA: i32 = 0x10;
const NARROW: i32 = -0x101;
const SHORTER: i32 = -0x102;
const SHORT: i32 = -0x103;
const LONG: i32 = -0x104;
const EXTRA_FIELD: i32 = 0x10000;
const MISSING_FIELD: i32 = 0x1000;

struct Row {
    ch: char,
    field: usize,
    ty: i32,
    min_len: usize,
}

const fn row(ch: char, field: usize, ty: i32, min_len: usize) -> Row {
    Row {
        ch,
        field,
        ty,
        min_len,
    }
}

/// `dtTypes`: the field, type and minimum length of each pattern character.
static TYPES: &[Row] = &[
    row('G', 0, SHORT, 1),
    row('G', 0, LONG, 4),
    row('G', 0, NARROW, 5),
    row('y', 1, NUMERIC, 1),
    row('Y', 1, NUMERIC + DELTA, 1),
    row('u', 1, NUMERIC + 2 * DELTA, 1),
    row('r', 1, NUMERIC + 3 * DELTA, 1),
    row('U', 1, SHORT, 1),
    row('U', 1, LONG, 4),
    row('U', 1, NARROW, 5),
    row('Q', 2, NUMERIC, 1),
    row('Q', 2, SHORT, 3),
    row('Q', 2, LONG, 4),
    row('Q', 2, NARROW, 5),
    row('q', 2, NUMERIC + DELTA, 1),
    row('q', 2, SHORT - DELTA, 3),
    row('q', 2, LONG - DELTA, 4),
    row('q', 2, NARROW - DELTA, 5),
    row('M', 3, NUMERIC, 1),
    row('M', 3, SHORT, 3),
    row('M', 3, LONG, 4),
    row('M', 3, NARROW, 5),
    row('L', 3, NUMERIC + DELTA, 1),
    row('L', 3, SHORT - DELTA, 3),
    row('L', 3, LONG - DELTA, 4),
    row('L', 3, NARROW - DELTA, 5),
    row('l', 3, NUMERIC + DELTA, 1),
    row('w', 4, NUMERIC, 1),
    row('W', 5, NUMERIC, 1),
    row('E', 6, SHORT, 1),
    row('E', 6, LONG, 4),
    row('E', 6, NARROW, 5),
    row('E', 6, SHORTER, 6),
    row('c', 6, NUMERIC + 2 * DELTA, 1),
    row('c', 6, SHORT - 2 * DELTA, 3),
    row('c', 6, LONG - 2 * DELTA, 4),
    row('c', 6, NARROW - 2 * DELTA, 5),
    row('c', 6, SHORTER - 2 * DELTA, 6),
    row('e', 6, NUMERIC + DELTA, 1),
    row('e', 6, SHORT - DELTA, 3),
    row('e', 6, LONG - DELTA, 4),
    row('e', 6, NARROW - DELTA, 5),
    row('e', 6, SHORTER - DELTA, 6),
    row('d', 9, NUMERIC, 1),
    row('g', 9, NUMERIC + DELTA, 1),
    row('D', 7, NUMERIC, 1),
    row('F', 8, NUMERIC, 1),
    row('a', 10, SHORT, 1),
    row('a', 10, LONG, 4),
    row('a', 10, NARROW, 5),
    row('b', 10, SHORT - DELTA, 1),
    row('b', 10, LONG - DELTA, 4),
    row('b', 10, NARROW - DELTA, 5),
    row('B', 10, SHORT - 3 * DELTA, 1),
    row('B', 10, LONG - 3 * DELTA, 4),
    row('B', 10, NARROW - 3 * DELTA, 5),
    row('H', 11, NUMERIC + 10 * DELTA, 1),
    row('k', 11, NUMERIC + 11 * DELTA, 1),
    row('h', 11, NUMERIC, 1),
    row('K', 11, NUMERIC + DELTA, 1),
    row('J', 11, NUMERIC + 5 * DELTA, 1),
    row('j', 11, NUMERIC + 6 * DELTA, 1),
    row('C', 11, NUMERIC + 7 * DELTA, 1),
    row('m', 12, NUMERIC, 1),
    row('s', 13, NUMERIC, 1),
    row('A', 13, NUMERIC + DELTA, 1),
    row('S', 14, NUMERIC, 1),
    row('v', 15, SHORT - 2 * DELTA, 1),
    row('v', 15, LONG - 2 * DELTA, 4),
    row('z', 15, SHORT, 1),
    row('z', 15, LONG, 4),
    row('Z', 15, NARROW - DELTA, 1),
    row('Z', 15, LONG - DELTA, 4),
    row('Z', 15, SHORT - DELTA, 5),
    row('O', 15, SHORT - DELTA, 1),
    row('O', 15, LONG - DELTA, 4),
    row('V', 15, SHORT - DELTA, 1),
    row('V', 15, LONG - DELTA, 2),
    row('V', 15, LONG - 1 - DELTA, 3),
    row('V', 15, LONG - 2 - DELTA, 4),
    row('X', 15, NARROW - DELTA, 1),
    row('X', 15, SHORT - DELTA, 2),
    row('X', 15, LONG - DELTA, 4),
    row('x', 15, NARROW - DELTA, 1),
    row('x', 15, SHORT - DELTA, 2),
    row('x', 15, LONG - DELTA, 4),
];

/// `FormatParser::getCanonicalIndex` (non-strict): the row of the longest matching minimum length.
fn canonical_row(item: &[char]) -> Option<&'static Row> {
    let ch = *item.first()?;
    if item.iter().any(|&c| c != ch) {
        return None;
    }
    let mut rows = TYPES
        .iter()
        .skip_while(|r| r.ch != ch)
        .take_while(|r| r.ch == ch);
    let mut best = rows.next()?;
    for r in rows {
        if r.min_len > item.len() {
            break;
        }
        best = r;
    }
    Some(best)
}

/// `FormatParser::set`: runs of one ASCII letter, or single other characters.
fn tokenize(pattern: &[char]) -> Vec<&[char]> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < pattern.len() && items.len() < 50 {
        let c = pattern[i];
        let mut end = i + 1;
        if c.is_ascii_alphabetic() {
            while end < pattern.len() && pattern[end] == c {
                end += 1;
            }
        }
        items.push(&pattern[i..end]);
        i = end;
    }
    items
}

/// `FormatParser::getQuoteLiteral`, starting at the opening quote `items[*i]`.
fn quote_literal(items: &[&[char]], i: &mut usize, out: &mut Vec<char>) {
    let mut j = *i;
    if items[j][0] == '\'' {
        out.extend_from_slice(items[j]);
        j += 1;
    }
    while j < items.len() {
        if items[j][0] == '\'' {
            if j + 1 < items.len() && items[j + 1][0] == '\'' {
                out.extend_from_slice(items[j]);
                out.extend_from_slice(items[j + 1]);
                j += 2;
                continue;
            }
            out.extend_from_slice(items[j]);
            break;
        }
        out.extend_from_slice(items[j]);
        j += 1;
    }
    *i = j;
}

/// `FormatParser::isPatternSeparator`, including its use of `items[i]` for the i-th character.
fn is_separator(field: &[char], items: &[&[char]]) -> bool {
    field.iter().enumerate().all(|(i, c)| {
        matches!(c, '\'' | '\\' | ' ' | ':' | '"' | ',' | '-')
            || items.get(i).is_some_and(|item| item[0] == '.')
    })
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Fields {
    chars: [char; FIELD_COUNT],
    lens: [u8; FIELD_COUNT],
}

impl Fields {
    fn set(&mut self, field: usize, ch: char, len: usize) {
        self.chars[field] = ch;
        self.lens[field] = len as u8;
    }

    fn clear(&mut self, field: usize) {
        self.set(field, '\0', 0);
    }

    fn is_empty(&self, field: usize) -> bool {
        self.lens[field] == 0
    }

    fn first_char(&self) -> Option<char> {
        (0..FIELD_COUNT)
            .find(|&f| self.lens[f] != 0)
            .map(|f| self.chars[f])
    }

    fn to_chars(&self) -> Vec<char> {
        (0..FIELD_COUNT)
            .flat_map(|f| std::iter::repeat_n(self.chars[f], self.lens[f] as usize))
            .collect()
    }
}

/// `PtnSkeleton`.
#[derive(Clone, Default, PartialEq)]
pub(super) struct Skeleton {
    ty: [i32; FIELD_COUNT],
    original: Fields,
    base: Fields,
}

impl Skeleton {
    /// `DateTimeMatcher::set`.
    fn of(pattern: &[char]) -> Skeleton {
        let mut s = Skeleton::default();
        let items = tokenize(pattern);
        let mut i = 0;
        while i < items.len() {
            let item = items[i];
            if item[0] == '\'' {
                quote_literal(&items, &mut i, &mut Vec::new());
                i += 1;
                continue;
            }
            if let Some(row) = canonical_row(item) {
                s.original.set(row.field, item[0], item.len());
                s.base.set(row.field, row.ch, row.min_len);
                s.ty[row.field] = if row.ty > 0 {
                    row.ty + item.len() as i32
                } else {
                    row.ty
                };
            }
            i += 1;
        }
        if !s.original.is_empty(MINUTE)
            && !s.original.is_empty(FRACTIONAL_SECOND)
            && s.original.is_empty(SECOND)
        {
            s.original.set(SECOND, 's', 1);
            s.base.set(SECOND, 's', 1);
            s.ty[SECOND] = NUMERIC + 1;
        }
        if !s.original.is_empty(HOUR) {
            if matches!(s.original.chars[HOUR], 'h' | 'K') {
                if s.original.is_empty(DAYPERIOD) {
                    s.original.set(DAYPERIOD, 'a', 1);
                    s.base.set(DAYPERIOD, 'a', 1);
                    s.ty[DAYPERIOD] = SHORT;
                }
            } else {
                s.original.clear(DAYPERIOD);
                s.base.clear(DAYPERIOD);
                s.ty[DAYPERIOD] = 0;
            }
        }
        s
    }

    fn field_mask(&self) -> u32 {
        (0..FIELD_COUNT)
            .filter(|&f| self.ty[f] != 0)
            .fold(0, |m, f| m | 1 << f)
    }

    /// `DateTimeMatcher::getDistance`: (distance, missing fields, extra fields).
    fn distance(&self, other: &Skeleton, include: u32) -> (i32, u32, u32) {
        let (mut result, mut missing, mut extra) = (0, 0, 0);
        for f in 0..FIELD_COUNT {
            let mine = if include & (1 << f) == 0 {
                0
            } else {
                self.ty[f]
            };
            let theirs = other.ty[f];
            if mine == theirs {
                continue;
            }
            if mine == 0 {
                result += EXTRA_FIELD;
                extra |= 1 << f;
            } else if theirs == 0 {
                result += MISSING_FIELD;
                missing |= 1 << f;
            } else {
                result += (mine - theirs).abs();
            }
        }
        (result, missing, extra)
    }

    /// The (pattern character, length) of each field, in field order.
    pub(super) fn fields(&self) -> impl Iterator<Item = (char, usize)> + '_ {
        (0..FIELD_COUNT)
            .filter(|&f| self.original.lens[f] != 0)
            .map(|f| (self.original.chars[f], self.original.lens[f] as usize))
    }
}

/// The fields of a pattern, for `resolvedOptions()` and hour cycle changes.
pub(super) fn pattern_skeleton(pattern: &str) -> Skeleton {
    Skeleton::of(&pattern.chars().collect::<Vec<_>>())
}

struct Elem {
    base: Vec<char>,
    skeleton: Skeleton,
    pattern: Vec<char>,
    specified: bool,
}

/// The data a generator is built from: the style patterns (time full..short, date full..short),
/// date-time combining patterns (full..short), appendItems, field names and availableFormats.
pub(super) struct LocaleData<'a> {
    pub styles: [&'a str; 8],
    pub glue: [&'a str; 4],
    pub append: [&'a str; FIELD_COUNT],
    pub names: [&'a str; FIELD_COUNT],
    pub formats: Vec<(&'a str, &'a str)>,
}

impl<'a> LocaleData<'a> {
    /// Parses one entry of `patterns.rs`.
    pub fn parse(data: &'a str) -> LocaleData<'a> {
        let mut parts = data.split('\x01');
        let mut next = || parts.next().unwrap_or("");
        let styles = std::array::from_fn(|_| next());
        let glue = std::array::from_fn(|_| next());
        let append = std::array::from_fn(|_| next());
        let names = std::array::from_fn(|_| next());
        let mut formats = Vec::new();
        while let (Some(skeleton), Some(pattern)) = (parts.next(), parts.next()) {
            formats.push((skeleton, pattern));
        }
        LocaleData {
            styles,
            glue,
            append,
            names,
            formats,
        }
    }
}

/// `DateTimePatternGenerator`.
pub(super) struct Generator {
    /// `PatternMap::boot`: elements by the first character of their base pattern.
    map: Vec<Vec<Elem>>,
    available: Vec<String>,
    glue: [Vec<char>; 4],
    append: [Vec<char>; FIELD_COUNT],
    names: [Vec<char>; FIELD_COUNT],
    default_hour: char,
    decimal: Vec<char>,
}

fn bucket(c: char) -> Option<usize> {
    match c {
        'A'..='Z' => Some(c as usize - 'A' as usize),
        'a'..='z' => Some(26 + c as usize - 'a' as usize),
        _ => None,
    }
}

impl Generator {
    /// `initData`. `override_formats` is false to keep the style patterns of a locale whose own
    /// availableFormats are not in the table (`formats` are then root's).
    pub fn new(
        data: &LocaleData,
        override_formats: bool,
        default_hour: char,
        decimal: &str,
    ) -> Generator {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        let mut g = Generator {
            map: (0..52).map(|_| Vec::new()).collect(),
            available: Vec::new(),
            glue: data.glue.map(chars),
            append: std::array::from_fn(|f| {
                if data.append[f].is_empty() {
                    chars("{0} \u{251C}{2}: {1}\u{2524}")
                } else {
                    chars(data.append[f])
                }
            }),
            names: std::array::from_fn(|f| {
                if data.names[f].is_empty() {
                    chars(&format!("F{f}"))
                } else {
                    chars(data.names[f])
                }
            }),
            default_hour,
            decimal: chars(decimal),
        };
        for c in "GyQMwWEDFdaHmsSv".chars() {
            g.add(&[c], None, false);
        }
        for i in 0..4 {
            g.add(&chars(data.styles[4 + i]), None, false);
            g.add(&chars(data.styles[i]), None, false);
        }
        for (skeleton, pattern) in &data.formats {
            if !g.available.iter().any(|s| s == skeleton) {
                g.available.push(skeleton.to_string());
                g.add(&chars(pattern), Some(&chars(skeleton)), override_formats);
            }
        }
        g
    }

    /// `addPatternWithSkeleton`.
    fn add(&mut self, pattern: &[char], skeleton_to_use: Option<&[char]>, override_: bool) {
        let skeleton = Skeleton::of(skeleton_to_use.unwrap_or(pattern));
        let base = skeleton.base.to_chars();
        let Some(index) = base.first().and_then(|&c| bucket(c)) else {
            return;
        };
        if let Some(dup) = self.map[index].iter().find(|e| e.base == base) {
            if (!dup.specified || (skeleton_to_use.is_some() && !override_)) && !override_ {
                return;
            }
        }
        if let Some(dup) = self.find(&skeleton) {
            if !override_ || (skeleton_to_use.is_some() && dup.specified) {
                return;
            }
        }
        let specified = skeleton_to_use.is_some();
        let list = &mut self.map[index];
        if let Some(dup) = list
            .iter_mut()
            .find(|e| e.base == base && e.skeleton.ty == skeleton.ty)
        {
            dup.pattern = pattern.to_vec();
            dup.specified = specified;
        } else {
            list.push(Elem {
                base,
                skeleton,
                pattern: pattern.to_vec(),
                specified,
            });
        }
    }

    /// `PatternMap::getPatternFromSkeleton` (by `original`).
    fn find(&self, skeleton: &Skeleton) -> Option<&Elem> {
        let index = bucket(skeleton.base.first_char()?)?;
        self.map[index]
            .iter()
            .find(|e| e.skeleton.original == skeleton.original)
    }

    /// `getBestRaw`: the closest element, and the fields it misses and adds.
    fn best_raw(&self, source: &Skeleton, include: u32) -> Option<(&Elem, u32, u32)> {
        let mut best: Option<(&Elem, u32, u32)> = None;
        let mut best_distance = i32::MAX;
        let mut best_missing: i64 = -1;
        for elem in self.map.iter().flatten() {
            let (distance, missing, extra) = source.distance(&elem.skeleton, include);
            if distance < best_distance
                || (distance == best_distance && best_missing < missing as i64)
            {
                best_distance = distance;
                best_missing = missing as i64;
                best = Some((elem, missing, extra));
                if distance == 0 {
                    break;
                }
            }
        }
        best
    }

    /// `adjustFieldTypes` with `UDATPG_MATCH_HOUR_FIELD_LENGTH`.
    fn adjust(
        &self,
        pattern: &[char],
        elem: &Elem,
        request: &Skeleton,
        fix_fractional: bool,
    ) -> Vec<char> {
        let specified = elem.specified.then_some(&elem.skeleton);
        let items = tokenize(pattern);
        let mut out = Vec::new();
        let mut i = 0;
        while i < items.len() {
            let field = items[i];
            if field[0] == '\'' {
                quote_literal(&items, &mut i, &mut out);
                i += 1;
                continue;
            }
            i += 1;
            if is_separator(field, &items) {
                out.extend_from_slice(field);
                continue;
            }
            let Some(row) = canonical_row(field) else {
                out.extend_from_slice(field);
                continue;
            };
            let ty = row.field;
            if fix_fractional && ty == SECOND {
                out.extend_from_slice(field);
                out.extend_from_slice(&self.decimal);
                let n = request.original.lens[FRACTIONAL_SECOND] as usize;
                out.extend(std::iter::repeat_n(
                    request.original.chars[FRACTIONAL_SECOND],
                    n,
                ));
                continue;
            }
            if request.ty[ty] == 0 {
                out.extend_from_slice(field);
                continue;
            }
            let req_char = request.original.chars[ty];
            let mut req_len = request.original.lens[ty] as usize;
            if req_char == 'E' && req_len < 3 {
                req_len = 3;
            }
            let mut adj_len = req_len;
            if ty == MINUTE || ty == SECOND {
                adj_len = field.len();
            } else if let Some(spec) = specified {
                if req_char != 'c' && req_char != 'e' {
                    let skel_len = spec.original.lens[ty] as usize;
                    let pat_numeric = row.ty > 0;
                    let skel_numeric = spec.ty[ty] > 0;
                    if skel_len == req_len || pat_numeric != skel_numeric {
                        adj_len = field.len();
                    }
                }
            }
            let keep_pattern_char =
                ty == HOUR || ty == MONTH || ty == WEEKDAY || (ty == YEAR && req_char != 'Y');
            let mut c = if keep_pattern_char {
                field[0]
            } else {
                req_char
            };
            if c == 'E' && adj_len < 3 {
                c = 'e';
            }
            if ty == HOUR {
                let d = self.default_hour;
                c = if req_char == d {
                    d
                } else {
                    match (req_char, d) {
                        ('h', 'K') => 'K',
                        ('H', 'k') => 'k',
                        ('k', 'H') => 'H',
                        ('K', 'h') => 'h',
                        _ => c,
                    }
                };
            }
            out.extend(std::iter::repeat_n(c, adj_len));
        }
        out
    }

    /// `getBestAppending`.
    fn best_appending(&self, request: &Skeleton, fields: u32) -> Vec<char> {
        if fields == 0 {
            return Vec::new();
        }
        let Some((elem, mut missing, _)) = self.best_raw(request, fields) else {
            return Vec::new();
        };
        let mut result = self.adjust(&elem.pattern, elem, request, false);
        let mut last_elem = elem;
        let mut last_missing = 0;
        while missing != 0 {
            if last_missing == missing {
                break;
            }
            if missing & SECOND_AND_FRACTIONAL_MASK == FRACTIONAL_MASK
                && fields & SECOND_AND_FRACTIONAL_MASK == SECOND_AND_FRACTIONAL_MASK
            {
                result = self.adjust(&result, last_elem, request, true);
                missing &= !FRACTIONAL_MASK;
                continue;
            }
            let starting = missing;
            let Some((elem, now_missing, _)) = self.best_raw(request, missing) else {
                break;
            };
            last_elem = elem;
            missing = now_missing;
            let temp = self.adjust(&elem.pattern, elem, request, false);
            let found = starting & !missing;
            let top = if found == 0 {
                0
            } else {
                (31 - found.leading_zeros() as usize).min(FIELD_COUNT - 1)
            };
            let mut name = vec!['\''];
            name.extend_from_slice(&self.names[top]);
            name.push('\'');
            result = substitute(&self.append[top], &[&result, &temp, &name]);
            last_missing = missing;
        }
        result
    }

    /// The date-time combining pattern of a date style (0 full to 3 short).
    pub fn glue(&self, style: usize) -> String {
        self.glue[style.min(3)].iter().collect()
    }

    /// `getBestPattern(skeleton, UDATPG_MATCH_HOUR_FIELD_LENGTH)`.
    pub fn best_pattern(&self, skeleton: &str) -> String {
        let request = Skeleton::of(&skeleton.chars().collect::<Vec<_>>());
        if let Some((elem, missing, extra)) = self.best_raw(&request, u32::MAX) {
            if missing == 0 && extra == 0 {
                return self
                    .adjust(&elem.pattern, elem, &request, false)
                    .into_iter()
                    .collect();
            }
        }
        let needed = request.field_mask();
        let date = self.best_appending(&request, needed & DATE_MASK);
        let time = self.best_appending(&request, needed & TIME_MASK);
        if date.is_empty() || time.is_empty() {
            return date.into_iter().chain(time).collect();
        }
        let style = match request.base.lens[MONTH] {
            4 if request.base.lens[WEEKDAY] > 0 => 0,
            4 => 1,
            3 => 2,
            _ => 3,
        };
        substitute(&self.glue[style], &[&time, &date])
            .into_iter()
            .collect()
    }
}

/// `SimpleFormatter::format` for patterns without quoting.
fn substitute(pattern: &[char], values: &[&[char]]) -> Vec<char> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        if pattern[i] == '{' && i + 2 < pattern.len() && pattern[i + 2] == '}' {
            if let Some(v) = pattern[i + 1]
                .to_digit(10)
                .and_then(|d| values.get(d as usize))
            {
                out.extend_from_slice(v);
                i += 3;
                continue;
            }
        }
        out.push(pattern[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::patterns;
    use super::*;

    fn generator(locale: &str) -> Generator {
        let (_, data) = patterns::LOCALES
            .iter()
            .find(|(id, _)| *id == locale)
            .unwrap();
        let hour = if locale.ends_with("_US") || locale.ends_with("_CN") {
            'h'
        } else {
            'H'
        };
        Generator::new(&LocaleData::parse(data), true, hour, ".")
    }

    #[test]
    fn best_patterns() {
        let en = generator("en_Latn_US");
        assert_eq!(en.best_pattern("yMd"), "M/d/y");
        assert_eq!(en.best_pattern("MMMdd"), "MMM dd");
        assert_eq!(en.best_pattern("yMdhms"), "M/d/y, h:mm:ss\u{202f}a");
        assert_eq!(en.best_pattern("hhmm"), "hh:mm\u{202f}a");
        assert_eq!(en.best_pattern("yMMMMd"), "MMMM d, y");
        assert_eq!(en.best_pattern("EEEEyMMMMd"), "EEEE, MMMM d, y");
        let zh = generator("zh_Hans_CN");
        assert_eq!(zh.best_pattern("yMd"), "y/M/d");
        assert_eq!(zh.best_pattern("yMdhms"), "y/M/d ah:mm:ss");
        assert_eq!(zh.best_pattern("Hm"), "HH:mm");
        assert_eq!(zh.best_pattern("MMMd"), "M月d日");
        let fr = generator("fr_Latn_FR");
        assert_eq!(fr.best_pattern("EEEEyMMMMdHm"), "EEEE d MMMM y 'à' HH:mm");
    }
}
