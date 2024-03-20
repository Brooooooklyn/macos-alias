#![deny(clippy::all)]

use std::ffi::OsStr;
use std::fs::Metadata;
use std::io::Cursor;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, path::Path};

use byteorder::BigEndian;
use byteorder::WriteBytesExt;
use core_foundation::{
  base::{kCFAllocatorDefault, kCFAllocatorNull, Boolean, CFIndex, CFIndexConvertible, TCFType},
  error::CFErrorRef,
  string::{
    kCFStringEncodingUTF8, CFString, CFStringCreateWithBytesNoCopy, CFStringGetCString,
    CFStringGetLength, CFStringGetMaximumSizeForEncoding, CFStringRef,
  },
  url::{kCFURLPOSIXPathStyle, kCFURLVolumeNameKey, CFURLCreateWithFileSystemPath, CFURLRef},
};
use napi::bindgen_prelude::*;
use napi_derive::napi;

// From 1904, 1, 1 to 1970, 1, 1
static APPLE_EPOCH: i64 = -2082844800000;

#[repr(u16)]
enum TargetType {
  File = 0,
  Directory = 1,
}

#[repr(u16)]
#[allow(dead_code)]
enum VolumeType {
  Local = 0,
  Network,
  Floppy400,
  Floppy800,
  Floppy1400,
  Other,
}

#[allow(dead_code)]
enum VolumeSignature {
  Bd,
  HPlus,
  Hx,
}

impl AsRef<str> for VolumeSignature {
  fn as_ref(&self) -> &str {
    match self {
      VolumeSignature::Bd => "BD",
      VolumeSignature::HPlus => "H+",
      VolumeSignature::Hx => "HX",
    }
  }
}

struct Info {
  version: u16,
  target: Target,
  volume: Volume,
  parent: Parent,
  extra: Vec<Extra>,
}

struct Target {
  type_: TargetType,
  filename: String,
  id: u32,
  created: SystemTime,
}

struct Volume {
  name: String,
  created: SystemTime,
  signature: VolumeSignature,
  type_: VolumeType,
}

struct Parent {
  id: u32,
  name: String,
}

struct Extra {
  type_: i16,
  length: u16,
  data: Vec<u8>,
}

fn apple_date(value: SystemTime) -> u32 {
  let since_the_epoch = value
    .duration_since(UNIX_EPOCH)
    .expect("Time went backwards");
  ((since_the_epoch.as_millis() as f64 - APPLE_EPOCH as f64) / 1000.0).round() as u32
}

fn encode(info: Info) -> Result<Vec<u8>> {
  let base_length = 150;
  let extra_length: usize = info
    .extra
    .iter()
    .map(|e| 4 + e.length as usize + (e.length % 2) as usize)
    .sum();
  let trailer_length = 4;

  let total = base_length + extra_length + trailer_length;
  let buf: Vec<u8> = vec![0; total];

  let mut cursor = Cursor::new(buf);

  cursor.write_u32::<BigEndian>(0)?;

  cursor.write_u16::<BigEndian>(total as u16)?;
  cursor.write_u16::<BigEndian>(info.version)?;

  cursor.write_u16::<BigEndian>(info.target.type_ as _)?;

  let vol_name_length = info.volume.name.len();
  if vol_name_length > 27 {
    return Err(Error::new(
      Status::GenericFailure,
      "Volume name is not longer than 27 chars",
    ));
  }

  cursor.write_u8(vol_name_length as u8)?;
  let padding = vec![0u8; 27 - info.volume.name.bytes().len()];

  cursor.write_all(info.volume.name.as_bytes())?;
  cursor.write_all(&padding)?;
  cursor.write_u32::<BigEndian>(apple_date(info.volume.created))?;
  let signature = info.volume.signature.as_ref().as_bytes();
  cursor.write_all(signature)?;
  cursor.write_u16::<BigEndian>(info.volume.type_ as _)?;
  cursor.write_u32::<BigEndian>(info.parent.id)?;

  let file_name_len = info.target.filename.len();
  if file_name_len > 63 {
    return Err(Error::new(
      Status::GenericFailure,
      "File name is not longer than 63 chars",
    ));
  }
  cursor.write_u8(file_name_len as u8)?;
  let filename_padding = vec![0u8; 63 - info.target.filename.bytes().len()];
  cursor.write_all(info.target.filename.as_bytes())?;
  cursor.write_all(&filename_padding)?;
  cursor.write_u32::<BigEndian>(info.target.id)?;
  cursor.write_u32::<BigEndian>(apple_date(info.target.created))?;

  let file_type_name = "\0\0\0\0";
  let file_creator_name = "\0\0\0\0";
  // I have only encountered 00 00 00 00
  cursor.write_all(file_type_name.as_bytes())?;
  cursor.write_all(file_creator_name.as_bytes())?;

  let nlvl_from: i16 = -1;
  let nlvl_to: i16 = -1;
  // I have only encountered -1
  cursor.write_i16::<BigEndian>(nlvl_from)?;
  cursor.write_i16::<BigEndian>(nlvl_to)?;

  let vol_attributes: u32 = 3330;
  cursor.write_u32::<BigEndian>(vol_attributes)?;

  let vol_fs_id: u16 = 0x0000;
  cursor.write_u16::<BigEndian>(vol_fs_id)?;

  let reserved_space = [0; 10];

  cursor.write_all(&reserved_space)?;
  for e in info.extra.iter() {
    cursor.write_i16::<BigEndian>(e.type_)?;
    cursor.write_u16::<BigEndian>(e.length)?;
    cursor.write_all(&e.data)?;

    if e.length % 2 == 1 {
      cursor.write_u8(0)?;
    }
  }

  cursor.write_i16::<BigEndian>(-1)?;
  cursor.write_u16::<BigEndian>(0)?;
  Ok(cursor.into_inner())
}

fn find_volume<'a, P: AsRef<OsStr> + ?Sized>(
  start_path: &'a P,
  start_stat: &'a Metadata,
) -> std::io::Result<&'a Path> {
  let mut last_dev = start_stat.dev();
  let mut last_ino = start_stat.ino();
  let mut last_path = Path::new(start_path);

  loop {
    if let Some(parent_path) = last_path.parent() {
      let parent_stat = fs::metadata(parent_path)?;

      if parent_stat.dev() != last_dev {
        return Ok(last_path);
      }

      if parent_stat.ino() == last_ino {
        return Ok(last_path);
      }

      last_dev = parent_stat.dev();
      last_ino = parent_stat.ino();
      last_path = parent_path;
    } else {
      return Ok(last_path);
    }
  }
}

fn utf16be(s: &str) -> Vec<u8> {
  let b: Vec<u16> = s.encode_utf16().collect();
  let mut result: Vec<u8> = Vec::new();
  for &number in &b {
    result.extend_from_slice(&number.to_be_bytes());
  }
  result
}

#[napi]
pub fn create(target_path: String) -> Result<Buffer> {
  let mut extra = Vec::new();

  let parent_path = Path::new(&target_path).parent().ok_or_else(|| {
    Error::new(
      Status::InvalidArg,
      "The target path has no parent directory.",
    )
  })?;
  let target_metadata = fs::metadata(&target_path)?;
  let parent_metadata = fs::metadata(parent_path)?;
  let volume_path = find_volume(&target_path, &target_metadata)?;
  let volume_metadata = fs::metadata(volume_path)?;

  assert!(target_metadata.is_file() || target_metadata.is_dir());

  let target = Target {
    id: target_metadata.ino() as u32,
    type_: if target_metadata.is_dir() {
      TargetType::Directory
    } else {
      TargetType::File
    },
    filename: Path::new(&target_path)
      .file_name()
      .unwrap()
      .to_str()
      .unwrap()
      .to_string(),
    created: UNIX_EPOCH + std::time::Duration::from_secs(target_metadata.ctime() as u64),
  };

  let parent = Parent {
    id: parent_metadata.ino() as u32,
    name: parent_path
      .file_name()
      .and_then(|s| s.to_str())
      .map(|s| s.to_string())
      .ok_or_else(|| Error::new(Status::InvalidArg, ""))?,
  };

  let volume = Volume {
    name: get_volume_name(volume_path.to_str().ok_or_else(|| {
      Error::new(
        Status::InvalidArg,
        "The volume path is not a valid UTF-8 string.",
      )
    })?),
    created: UNIX_EPOCH + std::time::Duration::from_secs(volume_metadata.ctime() as u64),
    signature: VolumeSignature::HPlus,
    type_: if volume_path.to_str() == Some("/") {
      VolumeType::Local
    } else {
      VolumeType::Other
    },
  };

  extra.push(Extra {
    type_: 0,
    length: parent.name.len() as u16,
    data: parent.name.as_bytes().to_vec(),
  });

  extra.push(Extra {
    type_: 1,
    length: 4,
    data: parent.id.to_be_bytes().to_vec(),
  });

  let filename_length = target.filename.len();
  let mut buffer = vec![0; 2 + filename_length * 2];
  buffer[0..2].copy_from_slice(&(filename_length as u16).to_be_bytes());
  buffer[2..].copy_from_slice(&utf16be(&target.filename));
  extra.push(Extra {
    type_: 14,
    length: buffer.len() as _,
    data: buffer,
  });

  let volume_name_length = volume.name.len();
  let mut buffer = vec![0; 2 + volume_name_length * 2];
  buffer[0..2].copy_from_slice(&(volume_name_length as u16).to_be_bytes());
  buffer[2..].copy_from_slice(&utf16be(&volume.name));
  extra.push(Extra {
    type_: 15,
    length: buffer.len() as _,
    data: buffer,
  });

  let volume_path_length = volume_path.to_string_lossy().len();

  let lp = &target_path[volume_path_length..];
  extra.push(Extra {
    type_: 18,
    length: lp.len() as u16,
    data: lp.as_bytes().to_vec(),
  });

  extra.push(Extra {
    type_: 19,
    length: volume_path_length as _,
    data: volume_path.to_string_lossy().as_bytes().to_vec(),
  });
  Ok(
    encode(Info {
      version: 2,
      target,
      volume,
      parent,
      extra,
    })?
    .into(),
  )
}

static FALSE: Boolean = false as Boolean;
static TRUE: Boolean = true as Boolean;

fn get_volume_name(path: &str) -> String {
  let a_string = unsafe {
    CFStringCreateWithBytesNoCopy(
      kCFAllocatorDefault,
      path.as_ptr(),
      path.len().to_CFIndex(),
      kCFStringEncodingUTF8,
      FALSE,
      kCFAllocatorNull,
    )
  };
  if a_string.is_null() {
    return String::new();
  }

  let url = unsafe {
    CFURLCreateWithFileSystemPath(kCFAllocatorDefault, a_string, kCFURLPOSIXPathStyle, TRUE)
  };

  let mut error = std::ptr::null_mut();
  let mut a_string = std::ptr::null();

  if unsafe { CFURLCopyResourcePropertyForKey(url, kCFURLVolumeNameKey, &mut a_string, &mut error) }
    == FALSE
  {
    return String::new();
  }

  let len: CFIndex = unsafe { CFStringGetLength(a_string) };
  let max_size: CFIndex = unsafe { CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) };
  let mut string = String::with_capacity(max_size as usize);
  if unsafe {
    CFStringGetCString(
      a_string,
      string.as_mut_ptr().cast(),
      max_size,
      kCFStringEncodingUTF8,
    ) == FALSE
  } {
    return String::new();
  }
  let string = unsafe { CFString::wrap_under_get_rule(a_string) };
  string.to_string()
}

extern "C" {
  pub fn CFURLCopyResourcePropertyForKey(
    url: CFURLRef,
    key: CFStringRef,
    propertyValueTypeRefPtr: *mut CFStringRef,
    error: *mut CFErrorRef,
  ) -> Boolean;
}

#[cfg(test)]
mod test {
  use base64::Engine;
  use std::time::{Duration, UNIX_EPOCH};

  const FIXTURE: &str = "AAAAAAEqAAIAAApUZXN0IFRpdGxlAAAAAAAAAAAAAAAAAAAAAADO615USCsABQAAABMMVGVzdEJrZy50aWZmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAFM7rXlgAAAAAAAAAAP////8AAA0CAAAAAAAAAAAAAAAAAAAACy5iYWNrZ3JvdW5kAAABAAQAAAATAAIAJFRlc3QgVGl0bGU6LmJhY2tncm91bmQ6AFRlc3RCa2cudGlmZgAPABYACgBUAGUAcwB0ACAAVABpAHQAbABlABIAGS8uYmFja2dyb3VuZC9UZXN0QmtnLnRpZmYAABMAEy9Wb2x1bWVzL1Rlc3QgVGl0bGUA//8AAA==";

  #[test]
  fn get_volume_name() {
    let name = super::get_volume_name("/");
    assert_eq!(name, "Macintosh HD");
  }

  #[test]
  fn decode() {
    let encoded = super::encode(super::Info {
      version: 2,
      volume: super::Volume {
        name: "Test Title".to_owned(),
        created: UNIX_EPOCH + Duration::from_millis(1388686804000),
        signature: crate::VolumeSignature::HPlus,
        type_: crate::VolumeType::Other,
      },
      parent: super::Parent {
        id: 19,
        name: ".background".to_owned(),
      },
      target: super::Target {
        id: 20,
        type_: super::TargetType::File,
        filename: "TestBkg.tiff".to_owned(),
        created: UNIX_EPOCH + Duration::from_millis(1388686808000),
      },
      extra: vec![
        super::Extra {
          type_: 0,
          length: 11,
          data: ".background".as_bytes().to_vec(),
        },
        super::Extra {
          type_: 1,
          length: 4,
          data: vec![0, 0, 0, 19],
        },
        super::Extra {
          type_: 2,
          length: 36,
          data: vec![
            84, 101, 115, 116, 32, 84, 105, 116, 108, 101, 58, 46, 98, 97, 99, 107, 103, 114, 111,
            117, 110, 100, 58, 0, 84, 101, 115, 116, 66, 107, 103, 46, 116, 105, 102, 102,
          ],
        },
        super::Extra {
          type_: 15,
          length: 22,
          data: vec![
            0, 10, 0, 84, 0, 101, 0, 115, 0, 116, 0, 32, 0, 84, 0, 105, 0, 116, 0, 108, 0, 101,
          ],
        },
        super::Extra {
          type_: 18,
          length: 25,
          data: vec![
            47, 46, 98, 97, 99, 107, 103, 114, 111, 117, 110, 100, 47, 84, 101, 115, 116, 66, 107,
            103, 46, 116, 105, 102, 102,
          ],
        },
        super::Extra {
          type_: 19,
          length: 19,
          data: vec![
            47, 86, 111, 108, 117, 109, 101, 115, 47, 84, 101, 115, 116, 32, 84, 105, 116, 108, 101,
          ],
        },
      ],
    })
    .expect("Should be able to encode");
    assert_eq!(
      base64::engine::general_purpose::STANDARD
        .decode(FIXTURE)
        .unwrap(),
      encoded
    );
  }
}
