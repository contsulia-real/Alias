//! 跨 emitter/runtime 的堆对象物理布局唯一 owner。
//!
//! 分配端与读取端必须消费同一组偏移；任何一侧单独改字段位置都会把合法 SSA
//! 变成越界读写或把一个对象字段解释成另一个字段。这里仅拥有物理布局，不拥有
//! array/iterator/closure/result/string 的语言语义。

use super::abi::{align_to, object_word_offset, value_layout, VTy, OBJECT_WORD_BYTES};
use crate::sema::hir::CtorKind;

pub(crate) const ARRAY_RAW_WORDS: i64 = 4;
pub(crate) const ARRAY_RAW_BYTES: i64 = ARRAY_RAW_WORDS * OBJECT_WORD_BYTES;
pub(crate) const ARRAY_DATA_OFFSET: i32 = object_word_offset(0);
pub(crate) const ARRAY_LEN_OFFSET: i32 = object_word_offset(1);
pub(crate) const ARRAY_CAP_OFFSET: i32 = object_word_offset(2);
pub(crate) const ARRAY_STRIDE_OFFSET: i32 = object_word_offset(3);

pub(crate) const ARRAY_WRAPPER_WORDS: i64 = 2;
pub(crate) const ARRAY_WRAPPER_RAW_OFFSET: i32 = object_word_offset(0);
pub(crate) const ARRAY_WRAPPER_VERSION_OFFSET: i32 = object_word_offset(1);

pub(crate) const ITERATOR_WORDS: i64 = 3;
pub(crate) const ITERATOR_ARRAY_OFFSET: i32 = object_word_offset(0);
pub(crate) const ITERATOR_INDEX_OFFSET: i32 = object_word_offset(1);
pub(crate) const ITERATOR_VERSION_OFFSET: i32 = object_word_offset(2);

pub(crate) const CLOSURE_WORDS: i64 = 2;
pub(crate) const CLOSURE_BYTES: i64 = CLOSURE_WORDS * OBJECT_WORD_BYTES;
pub(crate) const CLOSURE_CODE_OFFSET: i32 = object_word_offset(0);
pub(crate) const CLOSURE_ENV_OFFSET: i32 = object_word_offset(1);

pub(crate) const RESULT_TAG_OFFSET: i32 = object_word_offset(0);
pub(crate) const RESULT_OK_TAG: i64 = 0;
pub(crate) const RESULT_ERR_TAG: i64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResultLayout {
    pub(crate) payload_offset: i32,
    pub(crate) size: usize,
    pub(crate) align: usize,
}

/// Heap result root 的 canonical typed layout。两个 variant 共用同一 payload region，因此
/// offset 必须满足较强 payload alignment，region 必须容纳较大 payload，最终 root 再补齐
/// tail padding。构造、match、propagate、clone 与 shallow 若各自计算会造成跨 variant 越界。
pub(crate) fn result_layout(ok: &VTy, err: &VTy) -> ResultLayout {
    result_layout_from_payloads(value_layout(ok), value_layout(err))
}

fn result_layout_from_payloads(
    ok: super::abi::ValueLayout,
    err: super::abi::ValueLayout,
) -> ResultLayout {
    let payload_align = ok.align.max(err.align);
    let payload_size = ok.size.max(err.size);
    let payload_offset = align_to(OBJECT_WORD_BYTES as usize, payload_align);
    let align = (OBJECT_WORD_BYTES as usize).max(payload_align);
    ResultLayout {
        payload_offset: payload_offset as i32,
        size: align_to(payload_offset + payload_size, align),
        align,
    }
}

/// Result discriminant 是对象物理编码的一部分；构造、Pattern 和 `?` 必须经同一映射。
/// 若各 emitter 独立约定 0/1，合法对象会被其它路径反向解释为另一 variant。
pub(crate) const fn result_tag(kind: CtorKind) -> i64 {
    match kind {
        CtorKind::Ok => RESULT_OK_TAG,
        CtorKind::Err => RESULT_ERR_TAG,
    }
}

pub(crate) const STRING_WORDS: i64 = 3;
pub(crate) const STRING_BYTES: i64 = STRING_WORDS * OBJECT_WORD_BYTES;
pub(crate) const STRING_DATA_OFFSET: i32 = object_word_offset(0);
pub(crate) const STRING_LEN_OFFSET: i32 = object_word_offset(1);
// Display buffers may expose an interior data pointer, or immutable static bytes. Destruction
// must free the allocation base, never infer it from data/length or free a static address.
pub(crate) const STRING_ALLOCATION_OFFSET: i32 = object_word_offset(2);

/// Runtime provenance root shared by every view derived from one raw allocation. This layout is
/// compiler/runtime-internal rather than a user ABI, but allocation, pointer lowering and release
/// must still consume one owner instead of duplicating offsets.
pub(crate) const STORAGE_DESCRIPTOR_WORDS: i64 = 4;
pub(crate) const STORAGE_DESCRIPTOR_BYTES: i64 = STORAGE_DESCRIPTOR_WORDS * OBJECT_WORD_BYTES;
pub(crate) const STORAGE_DESCRIPTOR_BASE_OFFSET: i32 = object_word_offset(0);
pub(crate) const STORAGE_DESCRIPTOR_EXTENT_OFFSET: i32 = object_word_offset(1);
pub(crate) const STORAGE_DESCRIPTOR_KIND_OFFSET: i32 = object_word_offset(2);
pub(crate) const STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET: i32 = object_word_offset(3);
pub(crate) const STORAGE_KIND_RAW: i64 = 1;

/// Raw initialization metadata starts empty and is owned by its StorageDescriptor. Region entry
/// layout remains with the later typed-initialization phase; freezing it before a producer exists
/// would create a second, speculative type/destruction ABI.
pub(crate) const RAW_INIT_METADATA_WORDS: i64 = 2;
pub(crate) const RAW_INIT_METADATA_BYTES: i64 = RAW_INIT_METADATA_WORDS * OBJECT_WORD_BYTES;
pub(crate) const RAW_INIT_REGIONS_OFFSET: i32 = object_word_offset(0);
pub(crate) const RAW_INIT_REGION_COUNT_OFFSET: i32 = object_word_offset(1);

#[cfg(test)]
mod tests {
    use super::{
        result_layout_from_payloads, ResultLayout, RAW_INIT_METADATA_BYTES,
        RAW_INIT_METADATA_WORDS, RAW_INIT_REGIONS_OFFSET, RAW_INIT_REGION_COUNT_OFFSET,
        STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_BYTES, STORAGE_DESCRIPTOR_EXTENT_OFFSET,
        STORAGE_DESCRIPTOR_KIND_OFFSET, STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET,
        STORAGE_DESCRIPTOR_WORDS,
    };
    use crate::codegen::abi::ValueLayout;

    #[test]
    fn result_layout_reserves_the_larger_typed_payload_with_tail_padding() {
        let pointer = ValueLayout {
            size: 32,
            align: 8,
            stride: 32,
        };
        let narrow = ValueLayout {
            size: 1,
            align: 1,
            stride: 1,
        };
        assert_eq!(
            result_layout_from_payloads(pointer, narrow),
            ResultLayout {
                payload_offset: 8,
                size: 40,
                align: 8,
            }
        );
    }

    #[test]
    fn raw_storage_descriptor_and_metadata_offsets_have_one_word_owner() {
        assert_eq!(STORAGE_DESCRIPTOR_WORDS, 4);
        assert_eq!(STORAGE_DESCRIPTOR_BYTES, 32);
        assert_eq!(
            [
                STORAGE_DESCRIPTOR_BASE_OFFSET,
                STORAGE_DESCRIPTOR_EXTENT_OFFSET,
                STORAGE_DESCRIPTOR_KIND_OFFSET,
                STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET,
            ],
            [0, 8, 16, 24]
        );
        assert_eq!(RAW_INIT_METADATA_WORDS, 2);
        assert_eq!(RAW_INIT_METADATA_BYTES, 16);
        assert_eq!(
            [RAW_INIT_REGIONS_OFFSET, RAW_INIT_REGION_COUNT_OFFSET],
            [0, 8]
        );
    }
}
