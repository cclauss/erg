//! defines `Opcode` (represents Python bytecode opcodes).
//!
//! Opcode(Pythonバイトコードオペコードを表す)を定義する

#![allow(dead_code)]
#![allow(non_camel_case_types)]

use crate::impl_u8_enum;

impl_u8_enum! {Opcode311;
    CACHE = 0,
    POP_TOP = 1,
    PUSH_NULL = 2,
    NOP = 9,
    UNARY_POSITIVE = 10,
    UNARY_NEGATIVE = 11,
    UNARY_NOT = 12,
    UNARY_INVERT = 15,
    BINARY_SUBSCR = 25,
    GET_LEN = 30,
    MATCH_MAPPING = 31,
    MATCH_SEQUENCE = 32,
    MATCH_KEYS = 33,
    PUSH_EXC_INFO = 35,
    CHECK_EXC_MATCH = 36,
    CHECK_EG_MATCH = 37,
    WITH_EXCEPT_START = 49,
    GET_AITER = 50,
    GET_ANEXT = 51,
    BEFORE_ASYNC_WITH = 52,
    BEFORE_WITH = 53,
    END_ASYNC_FOR = 54,
    STORE_SUBSCR = 60,
    GET_ITER = 68,
    GET_YIELD_FROM_ITER = 69,
    PRINT_EXPR = 70,
    LOAD_BUILD_CLASS = 71,
    LOAD_ASSERTION_ERROR = 74,
    LIST_TO_TUPLE = 82,
    RETURN_VALUE = 83,
    IMPORT_STAR = 84,
    SETUP_ANNOTATIONS = 85,
    YIELD_VALUE = 86,
    ASYNC_GEN_WRAP = 87,
    PREP_RERAISE_STAR = 88,
    POP_EXCEPT = 89,
    /* ↓ These opcodes take an arg */
    STORE_NAME = 90,
    DELETE_NAME = 91,
    UNPACK_SEQUENCE = 92,
    FOR_ITER = 93,
    UNPACK_EX = 94,
    STORE_ATTR = 95,
    STORE_GLOBAL = 97,
    DELETE_GLOBAL = 98,
    SWAP = 99,
    LOAD_CONST = 100,
    LOAD_NAME = 101,
    BUILD_TUPLE = 102,
    BUILD_LIST = 103,
    BUILD_SET = 104,
    BUILD_MAP = 105, // build a Dict object
    LOAD_ATTR = 106,
    COMPARE_OP = 107,
    IMPORT_NAME = 108,
    IMPORT_FROM = 109,
    JUMP_FORWARD = 110,
    JUMP_IF_FALSE_OR_POP = 111,
    JUMP_IF_TRUE_OR_POP = 112,
    POP_JUMP_FORWARD_IF_FALSE = 114,
    POP_JUMP_FORWARD_IF_TRUE = 115,
    LOAD_GLOBAL = 116,
    IS_OP = 117,
    CONTAINS_OP = 118,
    RERAISE = 119,
    COPY = 120,
    BINARY_OP = 122,
    SEND = 123,
    LOAD_FAST = 124,
    STORE_FAST = 125,
    DELETE_FAST = 126,
    RAISE_VARARGS = 130,
    CALL_FUNCTION = 131,
    MAKE_FUNCTION = 132,
    MAKE_CELL = 135,
    LOAD_CLOSURE = 136,
    LOAD_DEREF = 137,
    STORE_DEREF = 138,
    JUMP_BACKWARD = 140,
    CALL_FUNCTION_EX = 142,
    EXTENDED_ARG = 144,
    LIST_APPEND = 145,
    SET_ADD = 146,
    MAP_ADD = 147,
    LOAD_CLASSDEREF = 148,
    COPY_FREE_VARS = 149,
    RESUME = 151,
    MATCH_CLASS = 152,
    FORMAT_VALUE = 155,
    LOAD_METHOD = 160,
    LIST_EXTEND = 162,
    PRECALL = 166,
    CALL = 171,
    KW_NAMES = 172,
    // Erg-specific opcodes (must have a unary `ERG_`)
    // Define in descending order from 219, 255
    ERG_POP_NTH = 196,
    ERG_PEEK_NTH = 197, // get ref to the arg-th element from TOS
    ERG_INC = 198,      // name += 1; arg: typecode
    ERG_DEC = 199,      // name -= 1
    ERG_LOAD_FAST_IMMUT = 200,
    ERG_STORE_FAST_IMMUT = 201,
    ERG_MOVE_FAST = 202,
    ERG_CLONE_FAST = 203,
    ERG_COPY_FAST = 204,
    ERG_REF_FAST = 205,
    ERG_REF_MUT_FAST = 206,
    ERG_MOVE_OUTER = 207,
    ERG_CLONE_OUTER = 208,
    ERG_COPY_OUTER = 209,
    ERG_REF_OUTER = 210,
    ERG_REF_MUT_OUTER = 211,
    ERG_LESS_THAN = 212,
    ERG_LESS_EQUAL = 213,
    ERG_EQUAL = 214,
    ERG_NOT_EQUAL = 215,
    ERG_MAKE_SLOT = 216,
    ERG_MAKE_TYPE = 217,
    ERG_MAKE_PURE_FUNCTION = 218,
    ERG_CALL_PURE_FUNCTION = 219,
    /* ↑ These opcodes take an arg ↑ */
    /* ↓ These opcodes take no arg ↓ */
    // ... = 220,
    ERG_LOAD_EMPTY_SLOT = 242,
    ERG_LOAD_EMPTY_STR = 243,
    ERG_LOAD_1_NAT = 244,
    ERG_LOAD_1_INT = 245,
    ERG_LOAD_1_REAL = 246,
    ERG_LOAD_NONE = 247,
    ERG_MUTATE = 248, // !x
    ERG_STORE_SUBSCR = 249, // `[] =` (it doesn't cause any exceptions)
    // ... = 250,
    ERG_BINARY_SUBSCR = 251, // `= []` (it doesn't cause any exceptions)
    ERG_BINARY_RANGE = 252,
    // `/?` (rhs may be 0, it may cause a runtime panic)
    ERG_TRY_BINARY_DIVIDE = 253,
    // `/` (rhs could not be 0, it doesn't cause any exceptions)
    ERG_BINARY_TRUE_DIVIDE = 254,
    NOT_IMPLEMENTED = 255,
}

impl_u8_enum! {BinOpCode;
    Add = 0,
    And = 1, // &
    FloorDiv = 2,
    LShift = 3,
    MatrixMultiply = 4,
    Multiply = 5,
    Remainder = 6,
    Or = 7, // |
    Power = 8,
    RShift = 9,
    Subtract = 10,
    TrueDivide = 11,
    Xor = 12,
    InplaceAdd = 13,
    InplaceAnd = 14,
    InplaceFloorDiv = 15,
    InplaceLShift = 16,
    InplaceMatrixMultiply = 17,
    InplaceMultiply = 18,
    InplaceRemainder = 19,
    InplaceOr = 20,
    InplacePower = 21,
    InplaceRShift = 22,
    InplaceSubtract = 23,
    InplaceTrueDivide = 24,
    InplaceXor = 25,
}
