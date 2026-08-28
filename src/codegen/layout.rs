//! 跨 emitter/runtime 的堆对象物理布局唯一 owner。
//!
//! 分配端与读取端必须消费同一组偏移；任何一侧单独改字段位置都会把合法 SSA
//! 变成越界读写或把一个对象字段解释成另一个字段。这里仅拥有物理布局，不拥有
//! 数组/迭代器/闭包/result 的语言语义。

use super::abi::{value_word_offset, VALUE_WORD_BYTES};

pub(crate) const ARRAY_RAW_WORDS: i64 = 3;
pub(crate) const ARRAY_RAW_BYTES: i64 = ARRAY_RAW_WORDS * VALUE_WORD_BYTES;
pub(crate) const ARRAY_DATA_OFFSET: i32 = value_word_offset(0);
pub(crate) const ARRAY_LEN_OFFSET: i32 = value_word_offset(1);
pub(crate) const ARRAY_CAP_OFFSET: i32 = value_word_offset(2);

pub(crate) const ARRAY_WRAPPER_WORDS: i64 = 2;
pub(crate) const ARRAY_WRAPPER_RAW_OFFSET: i32 = value_word_offset(0);
pub(crate) const ARRAY_WRAPPER_VERSION_OFFSET: i32 = value_word_offset(1);

pub(crate) const ITERATOR_WORDS: i64 = 3;
pub(crate) const ITERATOR_ARRAY_OFFSET: i32 = value_word_offset(0);
pub(crate) const ITERATOR_INDEX_OFFSET: i32 = value_word_offset(1);
pub(crate) const ITERATOR_VERSION_OFFSET: i32 = value_word_offset(2);

pub(crate) const CLOSURE_WORDS: i64 = 2;
pub(crate) const CLOSURE_BYTES: i64 = CLOSURE_WORDS * VALUE_WORD_BYTES;
pub(crate) const CLOSURE_CODE_OFFSET: i32 = value_word_offset(0);
pub(crate) const CLOSURE_ENV_OFFSET: i32 = value_word_offset(1);

pub(crate) const RESULT_WORDS: i64 = 2;
pub(crate) const RESULT_TAG_OFFSET: i32 = value_word_offset(0);
pub(crate) const RESULT_PAYLOAD_OFFSET: i32 = value_word_offset(1);
