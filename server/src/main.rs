//! Privastead Delivery Service (DS).
//! The DS is implemented as an HTTP server.
//! The DS is fully untrusted.
//!
//! Copyright (C) 2025  Ardalan Amiri Sani
//!
//! This program is free software: you can redistribute it and/or modify
//! it under the terms of the GNU General Public License as published by
//! the Free Software Foundation, either version 3 of the License, or
//! (at your option) any later version.
//!
//! This program is distributed in the hope that it will be useful,
//! but WITHOUT ANY WARRANTY; without even the implied warranty of
//! MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
//! GNU General Public License for more details.
//!
//! You should have received a copy of the GNU General Public License
//! along with this program.  If not, see <https://www.gnu.org/licenses/>.

#[macro_use]
extern crate rocket;

use std::io;
use std::path::Path;

use rocket::data::{Data, ToByteUnit};
use rocket::response::content::RawText;
use rocket::tokio::fs::{self, File};
use rocket::tokio::task;

mod auth;
use crate::auth::{initialize_users, BasicAuth};

mod fcm;
use crate::fcm::send_notification;

// Simple rate limiters for the server
const MAX_MOTION_FILE_SIZE: usize = 50; // in mebibytes
const MAX_NUM_PENDING_MOTION_FILES: usize = 100;
const MAX_LIVESTREAM_FILE_SIZE: usize = 20; // in mebibytes
const MAX_NUM_PENDING_LIVESTREAM_FILES: usize = 50;

async fn get_num_files(path: &Path) -> io::Result<usize> {
    let mut entries = fs::read_dir(path).await?;
    let mut num_files = 0;

    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            num_files += 1;
        }
    }

    Ok(num_files)
}

#[post("/<camera>/<filename>", data = "<data>")]
async fn upload(
    camera: &str,
    filename: &str,
    data: Data<'_>,
    auth: BasicAuth,
) -> io::Result<String> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    if !camera_path.exists() {
        fs::create_dir_all(&camera_path).await?;
    }

    let num_pending_files = get_num_files(&camera_path).await?;
    if num_pending_files > MAX_NUM_PENDING_MOTION_FILES {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: Reached max motion pending limit.",
        ));
    }

    let filepath = Path::new(&camera_path).join(filename);
    let filepath_tmp = Path::new(&camera_path).join(format!("{}_tmp", filename));

    data.open(MAX_MOTION_FILE_SIZE.mebibytes())
        .into_file(&filepath_tmp)
        .await?;
    // We write to a temp file first and then rename to avoid a race with the retrieve operation.
    fs::rename(filepath_tmp, filepath).await?;

    Ok("ok".to_string())
}

#[get("/<camera>/<filename>")]
async fn retrieve(camera: &str, filename: &str, auth: BasicAuth) -> Option<RawText<File>> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    let filepath = Path::new(&camera_path).join(filename);
    File::open(filepath).await.map(RawText).ok()
}

#[delete("/<camera>/<filename>")]
async fn delete_file(camera: &str, filename: &str, auth: BasicAuth) -> Option<()> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    let filepath = Path::new(&camera_path).join(filename);
    fs::remove_file(filepath).await.ok()
}

#[delete("/<camera>")]
async fn delete_camera(camera: &str, auth: BasicAuth) -> io::Result<()> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    fs::remove_dir_all(camera_path).await
}

#[post("/fcm_token", data = "<data>")]
async fn upload_fcm_token(data: Data<'_>, auth: BasicAuth) -> io::Result<String> {
    let root = format!("./{}/{}", "data", auth.username);
    let token_path = Path::new(&root).join("fcm_token");
    // FIXME: hardcoded max size
    data.open(5.kibibytes()).into_file(token_path).await?;
    Ok("ok".to_string())
}

#[post("/fcm_notification", data = "<data>")]
async fn send_fcm_notification(data: Data<'_>, auth: BasicAuth) -> io::Result<String> {
    let root = format!("./{}/{}", "data", auth.username);
    let token_path = Path::new(&root).join("fcm_token");
    if !token_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: FCM token not available.",
        ));
    }
    let token = fs::read_to_string(token_path).await?;

    // FIXME: hardcoded max size
    let notification_msg = data.open(8.kibibytes()).into_bytes().await?;
    task::block_in_place(|| {
        // FIXME: caller won't know if the notification failed to send
        let _ = send_notification(token, notification_msg.to_vec());
    });
    Ok("ok".to_string())
}

#[post("/livestream/<camera>")]
async fn livestream_start(camera: &str, auth: BasicAuth) -> io::Result<()> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    let livestream_path = Path::new(&camera_path).join("livestream");

    if livestream_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: There is a pending livestream start request.",
        ));
    }

    let update_path = Path::new(&camera_path).join("0");
    if update_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: Previous update has not been retrieved yet.",
        ));
    }

    fs::create_dir_all(&camera_path).await?;
    let _ = File::create(livestream_path).await?;

    Ok(())
}

#[get("/livestream/<camera>")]
async fn livestream_check(camera: &str, auth: BasicAuth) -> Option<()> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    let livestream_path = Path::new(&camera_path).join("livestream");
    if livestream_path.exists() {
        // wipe all the data from the previous stream (if any)
        // FIXME: error is ignored here and other uses of ok()
        fs::remove_dir_all(&camera_path).await.ok();
        fs::create_dir_all(&camera_path).await.ok();
        return Some(());
    }

    None
}

#[post("/livestream/<camera>/<filename>", data = "<data>")]
async fn livestream_upload(
    camera: &str,
    filename: &str,
    data: Data<'_>,
    auth: BasicAuth,
) -> io::Result<String> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    if !camera_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: Livestream session not started properly.",
        ));
    }

    let num_pending_files = get_num_files(&camera_path).await?;
    if num_pending_files > MAX_NUM_PENDING_LIVESTREAM_FILES {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Error: Reached max livestream pending limit.",
        ));
    }

    let filepath = Path::new(&camera_path).join(filename);
    let filepath_tmp = Path::new(&camera_path).join(format!("{}_tmp", filename));

    data.open(MAX_LIVESTREAM_FILE_SIZE.mebibytes())
        .into_file(&filepath_tmp)
        .await?;
    // We write to a temp file first and then rename to avoid a race with the retrieve operation.
    fs::rename(filepath_tmp, filepath).await?;

    // Returns the number of pending files
    Ok((num_pending_files + 1).to_string())
}

#[get("/livestream/<camera>/<filename>")]
async fn livestream_retrieve(
    camera: &str,
    filename: &str,
    auth: BasicAuth,
) -> Option<RawText<File>> {
    let root = format!("./{}/{}", "data", auth.username);
    let camera_path = Path::new(&root).join(camera);
    let filepath = Path::new(&camera_path).join(filename);
    if camera_path.exists() {
        let response = File::open(&filepath).await.map(RawText).ok();
        fs::remove_file(filepath).await.ok();
        return response;
    }

    None
}

#[launch]
fn rocket() -> _ {
    let config = rocket::Config {
        port: 8080,
        address: "0.0.0.0".parse().unwrap(),
        ..rocket::Config::default()
    };

    rocket::custom(config).manage(initialize_users()).mount(
        "/",
        routes![
            upload,
            retrieve,
            delete_file,
            delete_camera,
            upload_fcm_token,
            send_fcm_notification,
            livestream_start,
            livestream_check,
            livestream_upload,
            livestream_retrieve
        ],
    )
}
