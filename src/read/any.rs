use crate::alloc::borrow::Cow;
use crate::alloc::fmt;
use target_lexicon::{Architecture, BinaryFormat};
use uuid::Uuid;

#[cfg(feature = "wasm")]
use crate::read::wasm;
use crate::read::{coff, elf, macho, pe};
use crate::read::{
    FileFlags, Object, ObjectSection, ObjectSegment, Relocation, SectionFlags, SectionIndex,
    SectionKind, Symbol, SymbolIndex, SymbolMap,
};

/// Evaluate an expression on the contents of a file format enum.
///
/// This is a hack to avoid virtual calls.
macro_rules! with_inner {
    ($inner:expr, $enum:ident, | $var:ident | $body:expr) => {
        match $inner {
            $enum::Coff(ref $var) => $body,
            $enum::Elf32(ref $var) => $body,
            $enum::Elf64(ref $var) => $body,
            $enum::MachO32(ref $var) => $body,
            $enum::MachO64(ref $var) => $body,
            $enum::Pe(ref $var) => $body,
            #[cfg(feature = "wasm")]
            $enum::Wasm(ref $var) => $body,
        }
    };
}

macro_rules! with_inner_mut {
    ($inner:expr, $enum:ident, | $var:ident | $body:expr) => {
        match $inner {
            $enum::Coff(ref mut $var) => $body,
            $enum::Elf32(ref mut $var) => $body,
            $enum::Elf64(ref mut $var) => $body,
            $enum::MachO32(ref mut $var) => $body,
            $enum::MachO64(ref mut $var) => $body,
            $enum::Pe(ref mut $var) => $body,
            #[cfg(feature = "wasm")]
            $enum::Wasm(ref mut $var) => $body,
        }
    };
}

/// Like `with_inner!`, but wraps the result in another enum.
macro_rules! map_inner {
    ($inner:expr, $from:ident, $to:ident, | $var:ident | $body:expr) => {
        match $inner {
            $from::Coff(ref $var) => $to::Coff($body),
            $from::Elf32(ref $var) => $to::Elf32($body),
            $from::Elf64(ref $var) => $to::Elf64($body),
            $from::MachO32(ref $var) => $to::MachO32($body),
            $from::MachO64(ref $var) => $to::MachO64($body),
            $from::Pe(ref $var) => $to::Pe($body),
            #[cfg(feature = "wasm")]
            $from::Wasm(ref $var) => $to::Wasm($body),
        }
    };
}

/// Like `map_inner!`, but the result is a Result or Option.
macro_rules! map_inner_option {
    ($inner:expr, $from:ident, $to:ident, | $var:ident | $body:expr) => {
        match $inner {
            $from::Coff(ref $var) => $body.map($to::Coff),
            $from::Elf32(ref $var) => $body.map($to::Elf32),
            $from::Elf64(ref $var) => $body.map($to::Elf64),
            $from::MachO32(ref $var) => $body.map($to::MachO32),
            $from::MachO64(ref $var) => $body.map($to::MachO64),
            $from::Pe(ref $var) => $body.map($to::Pe),
            #[cfg(feature = "wasm")]
            $from::Wasm(ref $var) => $body.map($to::Wasm),
        }
    };
}

/// Call `next` for a file format iterator.
macro_rules! next_inner {
    ($inner:expr, $from:ident, $to:ident) => {
        match $inner {
            $from::Coff(ref mut iter) => iter.next().map($to::Coff),
            $from::Elf32(ref mut iter) => iter.next().map($to::Elf32),
            $from::Elf64(ref mut iter) => iter.next().map($to::Elf64),
            $from::MachO32(ref mut iter) => iter.next().map($to::MachO32),
            $from::MachO64(ref mut iter) => iter.next().map($to::MachO64),
            $from::Pe(ref mut iter) => iter.next().map($to::Pe),
            #[cfg(feature = "wasm")]
            $from::Wasm(ref mut iter) => iter.next().map($to::Wasm),
        }
    };
}

/// An object file.
///
/// Most functionality is provided by the `Object` trait implementation.
#[derive(Debug)]
pub struct File<'data> {
    inner: FileInternal<'data>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum FileInternal<'data> {
    Coff(coff::CoffFile<'data>),
    Elf32(elf::ElfFile32<'data>),
    Elf64(elf::ElfFile64<'data>),
    MachO32(macho::MachOFile32<'data>),
    MachO64(macho::MachOFile64<'data>),
    Pe(pe::PeFile<'data>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmFile),
}

impl<'data> File<'data> {
    /// Parse the raw file data.
    pub fn parse(data: &'data [u8]) -> Result<Self, &'static str> {
        if data.len() < 16 {
            return Err("File too short");
        }

        let inner = match [data[0], data[1], data[2], data[3], data[4]] {
            // 32-bit ELF
            [0x7f, b'E', b'L', b'F', 1] => FileInternal::Elf32(elf::ElfFile32::parse(data)?),
            // 64-bit ELF
            [0x7f, b'E', b'L', b'F', 2] => FileInternal::Elf64(elf::ElfFile64::parse(data)?),
            // 32-bit Mach-O
            [0xfe, 0xed, 0xfa, 0xce, _]
            | [0xce, 0xfa, 0xed, 0xfe, _] => FileInternal::MachO32(macho::MachOFile32::parse(data)?),
            // 64-bit Mach-O
            | [0xfe, 0xed, 0xfa, 0xcf, _]
            | [0xcf, 0xfa, 0xed, 0xfe, _] => FileInternal::MachO64(macho::MachOFile64::parse(data)?),
            // WASM
            #[cfg(feature = "wasm")]
            [0x00, b'a', b's', b'm', _] => FileInternal::Wasm(wasm::WasmFile::parse(data)?),
            // MS-DOS, assume stub for Windows PE
            [b'M', b'Z', _, _, _] => FileInternal::Pe(pe::PeFile::parse(data)?),
            // TODO: more COFF machines
            // COFF x86
            [0x4c, 0x01, _, _, _]
            // COFF x86-64
            | [0x64, 0x86, _, _, _] => FileInternal::Coff(coff::CoffFile::parse(data)?),
            _ => return Err("Unknown file magic"),
        };
        Ok(File { inner })
    }

    /// Return the file format.
    pub fn format(&self) -> BinaryFormat {
        match self.inner {
            FileInternal::Elf32(_) | FileInternal::Elf64(_) => BinaryFormat::Elf,
            FileInternal::MachO32(_) | FileInternal::MachO64(_) => BinaryFormat::Macho,
            FileInternal::Coff(_) | FileInternal::Pe(_) => BinaryFormat::Coff,
            #[cfg(feature = "wasm")]
            FileInternal::Wasm(_) => BinaryFormat::Wasm,
        }
    }
}

impl<'data, 'file> Object<'data, 'file> for File<'data>
where
    'data: 'file,
{
    type Segment = Segment<'data, 'file>;
    type SegmentIterator = SegmentIterator<'data, 'file>;
    type Section = Section<'data, 'file>;
    type SectionIterator = SectionIterator<'data, 'file>;
    type SymbolIterator = SymbolIterator<'data, 'file>;

    fn architecture(&self) -> Architecture {
        with_inner!(self.inner, FileInternal, |x| x.architecture())
    }

    fn is_little_endian(&self) -> bool {
        with_inner!(self.inner, FileInternal, |x| x.is_little_endian())
    }

    fn is_64(&self) -> bool {
        with_inner!(self.inner, FileInternal, |x| x.is_64())
    }

    fn segments(&'file self) -> SegmentIterator<'data, 'file> {
        SegmentIterator {
            inner: map_inner!(self.inner, FileInternal, SegmentIteratorInternal, |x| x
                .segments()),
        }
    }

    fn section_by_name(&'file self, section_name: &str) -> Option<Section<'data, 'file>> {
        map_inner_option!(self.inner, FileInternal, SectionInternal, |x| x
            .section_by_name(section_name))
        .map(|inner| Section { inner })
    }

    fn section_by_index(&'file self, index: SectionIndex) -> Option<Section<'data, 'file>> {
        map_inner_option!(self.inner, FileInternal, SectionInternal, |x| x
            .section_by_index(index))
        .map(|inner| Section { inner })
    }

    fn section_data_by_name(&self, section_name: &str) -> Option<Cow<'data, [u8]>> {
        with_inner!(self.inner, FileInternal, |x| x
            .section_data_by_name(section_name))
    }

    fn sections(&'file self) -> SectionIterator<'data, 'file> {
        SectionIterator {
            inner: map_inner!(self.inner, FileInternal, SectionIteratorInternal, |x| x
                .sections()),
        }
    }

    fn symbol_by_index(&self, index: SymbolIndex) -> Option<Symbol<'data>> {
        with_inner!(self.inner, FileInternal, |x| x.symbol_by_index(index))
    }

    fn symbols(&'file self) -> SymbolIterator<'data, 'file> {
        SymbolIterator {
            inner: map_inner!(self.inner, FileInternal, SymbolIteratorInternal, |x| x
                .symbols()),
        }
    }

    fn dynamic_symbols(&'file self) -> SymbolIterator<'data, 'file> {
        SymbolIterator {
            inner: map_inner!(self.inner, FileInternal, SymbolIteratorInternal, |x| x
                .dynamic_symbols()),
        }
    }

    fn symbol_map(&self) -> SymbolMap<'data> {
        with_inner!(self.inner, FileInternal, |x| x.symbol_map())
    }

    fn has_debug_symbols(&self) -> bool {
        with_inner!(self.inner, FileInternal, |x| x.has_debug_symbols())
    }

    #[inline]
    fn mach_uuid(&self) -> Option<Uuid> {
        with_inner!(self.inner, FileInternal, |x| x.mach_uuid())
    }

    #[inline]
    fn build_id(&self) -> Option<&'data [u8]> {
        with_inner!(self.inner, FileInternal, |x| x.build_id())
    }

    #[inline]
    fn gnu_debuglink(&self) -> Option<(&'data [u8], u32)> {
        with_inner!(self.inner, FileInternal, |x| x.gnu_debuglink())
    }

    fn entry(&self) -> u64 {
        with_inner!(self.inner, FileInternal, |x| x.entry())
    }

    fn flags(&self) -> FileFlags {
        with_inner!(self.inner, FileInternal, |x| x.flags())
    }
}

/// An iterator over the segments of a `File`.
#[derive(Debug)]
pub struct SegmentIterator<'data, 'file>
where
    'data: 'file,
{
    inner: SegmentIteratorInternal<'data, 'file>,
}

#[derive(Debug)]
enum SegmentIteratorInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffSegmentIterator<'data, 'file>),
    Elf32(elf::ElfSegmentIterator32<'data, 'file>),
    Elf64(elf::ElfSegmentIterator64<'data, 'file>),
    MachO32(macho::MachOSegmentIterator32<'data, 'file>),
    MachO64(macho::MachOSegmentIterator64<'data, 'file>),
    Pe(pe::PeSegmentIterator<'data, 'file>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmSegmentIterator<'file>),
}

impl<'data, 'file> Iterator for SegmentIterator<'data, 'file> {
    type Item = Segment<'data, 'file>;

    fn next(&mut self) -> Option<Self::Item> {
        next_inner!(self.inner, SegmentIteratorInternal, SegmentInternal)
            .map(|inner| Segment { inner })
    }
}

/// A segment of a `File`.
pub struct Segment<'data, 'file>
where
    'data: 'file,
{
    inner: SegmentInternal<'data, 'file>,
}

#[derive(Debug)]
enum SegmentInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffSegment<'data, 'file>),
    Elf32(elf::ElfSegment32<'data, 'file>),
    Elf64(elf::ElfSegment64<'data, 'file>),
    MachO32(macho::MachOSegment32<'data, 'file>),
    MachO64(macho::MachOSegment64<'data, 'file>),
    Pe(pe::PeSegment<'data, 'file>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmSegment<'file>),
}

impl<'data, 'file> fmt::Debug for Segment<'data, 'file> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // It's painful to do much better than this
        f.debug_struct("Segment")
            .field("name", &self.name().unwrap_or("<unnamed>"))
            .field("address", &self.address())
            .field("size", &self.data().len())
            .finish()
    }
}

impl<'data, 'file> ObjectSegment<'data> for Segment<'data, 'file> {
    fn address(&self) -> u64 {
        with_inner!(self.inner, SegmentInternal, |x| x.address())
    }

    fn size(&self) -> u64 {
        with_inner!(self.inner, SegmentInternal, |x| x.size())
    }

    fn align(&self) -> u64 {
        with_inner!(self.inner, SegmentInternal, |x| x.align())
    }

    fn file_range(&self) -> (u64, u64) {
        with_inner!(self.inner, SegmentInternal, |x| x.file_range())
    }

    fn data(&self) -> &'data [u8] {
        with_inner!(self.inner, SegmentInternal, |x| x.data())
    }

    fn data_range(&self, address: u64, size: u64) -> Option<&'data [u8]> {
        with_inner!(self.inner, SegmentInternal, |x| x.data_range(address, size))
    }

    fn name(&self) -> Option<&str> {
        with_inner!(self.inner, SegmentInternal, |x| x.name())
    }
}

/// An iterator of the sections of a `File`.
#[derive(Debug)]
pub struct SectionIterator<'data, 'file>
where
    'data: 'file,
{
    inner: SectionIteratorInternal<'data, 'file>,
}

// we wrap our enums in a struct so that they are kept private.
#[derive(Debug)]
enum SectionIteratorInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffSectionIterator<'data, 'file>),
    Elf32(elf::ElfSectionIterator32<'data, 'file>),
    Elf64(elf::ElfSectionIterator64<'data, 'file>),
    MachO32(macho::MachOSectionIterator32<'data, 'file>),
    MachO64(macho::MachOSectionIterator64<'data, 'file>),
    Pe(pe::PeSectionIterator<'data, 'file>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmSectionIterator<'file>),
}

impl<'data, 'file> Iterator for SectionIterator<'data, 'file> {
    type Item = Section<'data, 'file>;

    fn next(&mut self) -> Option<Self::Item> {
        next_inner!(self.inner, SectionIteratorInternal, SectionInternal)
            .map(|inner| Section { inner })
    }
}

/// A Section of a File
pub struct Section<'data, 'file>
where
    'data: 'file,
{
    inner: SectionInternal<'data, 'file>,
}

enum SectionInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffSection<'data, 'file>),
    Elf32(elf::ElfSection32<'data, 'file>),
    Elf64(elf::ElfSection64<'data, 'file>),
    MachO32(macho::MachOSection32<'data, 'file>),
    MachO64(macho::MachOSection64<'data, 'file>),
    Pe(pe::PeSection<'data, 'file>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmSection<'file>),
}

impl<'data, 'file> fmt::Debug for Section<'data, 'file> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // It's painful to do much better than this
        let mut s = f.debug_struct("Section");
        if let Some(segment) = self.segment_name() {
            s.field("segment", &segment);
        }
        s.field("name", &self.name().unwrap_or("<invalid name>"))
            .field("address", &self.address())
            .field("size", &self.size())
            .field("kind", &self.kind())
            .finish()
    }
}

impl<'data, 'file> ObjectSection<'data> for Section<'data, 'file> {
    type RelocationIterator = RelocationIterator<'data, 'file>;

    fn index(&self) -> SectionIndex {
        with_inner!(self.inner, SectionInternal, |x| x.index())
    }

    fn address(&self) -> u64 {
        with_inner!(self.inner, SectionInternal, |x| x.address())
    }

    fn size(&self) -> u64 {
        with_inner!(self.inner, SectionInternal, |x| x.size())
    }

    fn align(&self) -> u64 {
        with_inner!(self.inner, SectionInternal, |x| x.align())
    }

    fn file_range(&self) -> Option<(u64, u64)> {
        with_inner!(self.inner, SectionInternal, |x| x.file_range())
    }

    fn data(&self) -> Cow<'data, [u8]> {
        with_inner!(self.inner, SectionInternal, |x| x.data())
    }

    fn data_range(&self, address: u64, size: u64) -> Option<&'data [u8]> {
        with_inner!(self.inner, SectionInternal, |x| x.data_range(address, size))
    }

    fn uncompressed_data(&self) -> Cow<'data, [u8]> {
        with_inner!(self.inner, SectionInternal, |x| x.uncompressed_data())
    }

    fn name(&self) -> Option<&str> {
        with_inner!(self.inner, SectionInternal, |x| x.name())
    }

    fn segment_name(&self) -> Option<&str> {
        with_inner!(self.inner, SectionInternal, |x| x.segment_name())
    }

    fn kind(&self) -> SectionKind {
        with_inner!(self.inner, SectionInternal, |x| x.kind())
    }

    fn relocations(&self) -> RelocationIterator<'data, 'file> {
        RelocationIterator {
            inner: map_inner!(
                self.inner,
                SectionInternal,
                RelocationIteratorInternal,
                |x| x.relocations()
            ),
        }
    }

    fn flags(&self) -> SectionFlags {
        with_inner!(self.inner, SectionInternal, |x| x.flags())
    }
}

/// An iterator over symbol table entries.
#[derive(Debug)]
pub struct SymbolIterator<'data, 'file>
where
    'data: 'file,
{
    inner: SymbolIteratorInternal<'data, 'file>,
}

#[derive(Debug)]
enum SymbolIteratorInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffSymbolIterator<'data, 'file>),
    Elf32(elf::ElfSymbolIterator32<'data, 'file>),
    Elf64(elf::ElfSymbolIterator64<'data, 'file>),
    MachO32(macho::MachOSymbolIterator32<'data, 'file>),
    MachO64(macho::MachOSymbolIterator64<'data, 'file>),
    Pe(pe::PeSymbolIterator<'data, 'file>),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmSymbolIterator<'file>),
}

impl<'data, 'file> Iterator for SymbolIterator<'data, 'file> {
    type Item = (SymbolIndex, Symbol<'data>);

    fn next(&mut self) -> Option<Self::Item> {
        with_inner_mut!(self.inner, SymbolIteratorInternal, |x| x.next())
    }
}

/// An iterator over relocation entries
#[derive(Debug)]
pub struct RelocationIterator<'data, 'file>
where
    'data: 'file,
{
    inner: RelocationIteratorInternal<'data, 'file>,
}

#[derive(Debug)]
enum RelocationIteratorInternal<'data, 'file>
where
    'data: 'file,
{
    Coff(coff::CoffRelocationIterator<'data, 'file>),
    Elf32(elf::ElfRelocationIterator32<'data, 'file>),
    Elf64(elf::ElfRelocationIterator64<'data, 'file>),
    MachO32(macho::MachORelocationIterator32<'data, 'file>),
    MachO64(macho::MachORelocationIterator64<'data, 'file>),
    Pe(pe::PeRelocationIterator),
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmRelocationIterator),
}

impl<'data, 'file> Iterator for RelocationIterator<'data, 'file> {
    type Item = (u64, Relocation);

    fn next(&mut self) -> Option<Self::Item> {
        with_inner_mut!(self.inner, RelocationIteratorInternal, |x| x.next())
    }
}
