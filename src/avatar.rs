use std::io::Cursor;
use std::path::Path;

use image::DynamicImage;
use image::ImageFormat;
use image::ImageReader;
use image::imageops::FilterType;
use sha2::Digest;
use sha2::Sha256;
use sqlx::PgPool;
use thiserror::Error;
use tokio::task::JoinError;

use crate::store::UPLOADS_DIR;

pub const ICONS: [&str; 30] = [
    "cat",
    "dog",
    "rabbit",
    "bird",
    "fish",
    "turtle",
    "squirrel",
    "bug",
    "rocket",
    "ghost",
    "crown",
    "star",
    "heart",
    "zap",
    "leaf",
    "flower-2",
    "trees",
    "mountain",
    "sun-medium",
    "moon",
    "cloud",
    "snowflake",
    "flame-kindling",
    "coffee",
    "pizza",
    "music",
    "gamepad-2",
    "bike",
    "dumbbell",
    "book-open",
];

pub const PALETTE_SIZE: u8 = 10;
pub const AVATAR_SIZE: u32 = 256;
const MAX_DIMENSION: u32 = 8192;
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const PNG_EXTENSION: &str = "png";

#[derive(Debug, Error)]
pub enum AvatarError {
    #[error("unsupported image format")]
    Unsupported,
    #[error("image exceeds decoder limits")]
    TooLarge,
    #[error("failed to decode image")]
    Decode,
    #[error("failed to encode avatar")]
    Encode,
    #[error("background task failed")]
    Task,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct ProcessedAvatar {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

pub fn is_valid_icon(icon: &str) -> bool {
    return ICONS.contains(&icon);
}

pub fn is_valid_color(color: i64) -> bool {
    return (0..=i64::from(PALETTE_SIZE - 1)).contains(&color);
}

pub async fn process_photo(bytes: Vec<u8>) -> Result<ProcessedAvatar, AvatarError> {
    let result = tokio::task::spawn_blocking(move || process_sync(bytes)).await;
    return result.map_err(|_: JoinError| AvatarError::Task)?;
}

pub async fn save_photo(
    pool: &PgPool,
    user_id: i64,
    processed: ProcessedAvatar,
) -> Result<(), AvatarError> {
    tokio::fs::create_dir_all(UPLOADS_DIR).await?;
    let path = Path::new(UPLOADS_DIR).join(&processed.file_name);
    let exists = matches!(tokio::fs::try_exists(&path).await, Ok(true));
    if !exists {
        tokio::fs::write(&path, &processed.bytes).await?;
    }
    set_photo(pool, user_id, &processed.file_name).await?;
    return Ok(());
}

fn process_sync(bytes: Vec<u8>) -> Result<ProcessedAvatar, AvatarError> {
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| AvatarError::Decode)?;
    let format = reader.format().ok_or(AvatarError::Unsupported)?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP => {}
        _ => return Err(AvatarError::Unsupported),
    }
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC_BYTES);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(image::ImageError::Limits(_)) => return Err(AvatarError::TooLarge),
        Err(_) => return Err(AvatarError::Decode),
    };
    let encoded = encode_png(crop_square(image))?;
    let file_name = format!(
        "{}.{}",
        hex_encode(&Sha256::digest(&encoded)),
        PNG_EXTENSION
    );
    return Ok(ProcessedAvatar {
        file_name,
        bytes: encoded,
    });
}

fn crop_square(image: DynamicImage) -> DynamicImage {
    let width = image.width();
    let height = image.height();
    let side = width.min(height);
    let x = (width - side) / 2;
    let y = (height - side) / 2;
    let cropped = image.crop_imm(x, y, side, side);
    return cropped.resize_exact(AVATAR_SIZE, AVATAR_SIZE, FilterType::Lanczos3);
}

fn encode_png(image: DynamicImage) -> Result<Vec<u8>, AvatarError> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|_| AvatarError::Encode)?;
    return Ok(out);
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    return out;
}

pub async fn set_photo(
    pool: &PgPool,
    user_id: i64,
    file_name: &str,
) -> Result<(), AvatarError> {
    sqlx::query("UPDATE users SET avatar_path = $1, avatar_icon = NULL WHERE id = $2")
        .bind(file_name)
        .bind(user_id)
        .execute(pool)
        .await?;
    return Ok(());
}

pub async fn set_icon(
    pool: &PgPool,
    user_id: i64,
    icon: &str,
    color: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET avatar_icon = $1, avatar_color = $2, avatar_path = NULL WHERE id = $3",
    )
    .bind(icon)
    .bind(color)
    .bind(user_id)
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn clear(pool: &PgPool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET avatar_path = NULL, avatar_icon = NULL, avatar_color = NULL WHERE id = $1",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    return Ok(());
}
