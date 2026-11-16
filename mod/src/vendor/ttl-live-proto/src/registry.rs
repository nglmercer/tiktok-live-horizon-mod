//! Descriptor registry for the pinned schema.
//!
//! Generated structs answer "decode these bytes as `WebcastChatMessage`". This
//! registry answers the different question the page-owned WebSocket path asks:
//! "what is field 7 of whatever this method is called?" — including for methods
//! TikTok shipped after the pin, where no generated struct exists.
//!
//! It is plain data with no dependencies. The decoder that walks it lives in
//! `ttl-live-events`.

/// Protobuf wire representation expected for a schema field.
///
/// This is what the dynamic decoder switches on: several logical types share
/// one wire category (every integer and bool is a varint on the wire), so a
/// consumer that needs the declared type reads [`FieldSchema::value_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Varint,
    Fixed64,
    Fixed32,
    String,
    Bytes,
    /// Fully qualified name of the nested message type.
    Message(&'static str),
}

/// Logical protobuf type of a schema field, as declared in the `.proto` source.
///
/// Unlike [`FieldKind`], nothing is collapsed to a wire category: `int64` and
/// `uint64` stay distinct, and message and enum references keep the fully
/// qualified name of the type they point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValueKind {
    Double,
    Float,
    Int64,
    Uint64,
    Int32,
    Fixed64,
    Fixed32,
    Bool,
    String,
    /// Fully qualified name of the nested message type.
    Message(&'static str),
    Bytes,
    Uint32,
    /// Fully qualified name of the enum type.
    Enum(&'static str),
    Sfixed32,
    Sfixed64,
    Sint32,
    Sint64,
}

/// How many values a schema field holds, from the descriptor label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldCardinality {
    Optional,
    Required,
    Repeated,
}

/// Descriptor for one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSchema {
    pub number: u32,
    pub name: &'static str,
    /// JSON name from the descriptor (`display_id` -> `displayId`).
    pub json_name: &'static str,
    pub kind: FieldKind,
    pub value_kind: FieldValueKind,
    pub cardinality: FieldCardinality,
    pub is_map: bool,
    pub oneof_name: Option<&'static str>,
    pub oneof_index: Option<i32>,
    pub deprecated: bool,
}

/// Descriptor for one protobuf message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageSchema {
    /// Fully qualified protobuf name, e.g. `webcast.model.message.WebcastChatMessage`.
    pub name: &'static str,
    pub fields: &'static [FieldSchema],
    pub is_map_entry: bool,
}

impl MessageSchema {
    /// Descriptor for a field number, if the schema declares one.
    pub fn field(&self, number: u32) -> Option<&'static FieldSchema> {
        self.fields.iter().find(|field| field.number == number)
    }
}

include!("generated/schema_registry.rs");


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumValueSchema {
    pub number: i32,
    pub name: &'static str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumSchema {
    pub name: &'static str,
    pub values: &'static [EnumValueSchema],
}
pub fn enum_by_name(name: &str) -> Option<&'static EnumSchema> {
    ENUMS.iter().find(|e| e.name == name)
}
// DESCRIPTOR_BYTES is not vendored: only ttl-live-native uses it.
