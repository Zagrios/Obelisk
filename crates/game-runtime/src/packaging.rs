//! Build-time acquisition from pinned public Valve packages. Never searches for
//! Steam, executes an installer or accesses account data.
use crate::{
    Error,
    package::{self, SourcePackage},
};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

async fn download(
    client: &reqwest::Client,
    source: &SourcePackage,
    directory: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = directory.join(format!("{}.zip", source.sha256));
    if path.is_file()
        && path.metadata()?.len() == source.size
        && crate::sha(&path)? == source.sha256
    {
        return Ok(path);
    }
    let mut response = client.get(&source.url).send().await?.error_for_status()?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    let mut size = 0_u64;
    while let Some(bytes) = response.chunk().await? {
        size += bytes.len() as u64;
        if size > source.size {
            return Err("Valve package exceeds pinned size".into());
        }
        temporary.write_all(&bytes)?;
    }
    temporary.as_file().sync_all()?;
    if size != source.size || crate::sha(temporary.path())? != source.sha256 {
        return Err("Valve package integrity mismatch".into());
    }
    temporary.persist(&path)?;
    Ok(path)
}

pub async fn build(downloads: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if package::verify_archive(output).is_ok() {
        return Ok(());
    }
    fs::create_dir_all(downloads)?;
    let parent = output
        .parent()
        .ok_or("Package output requires a parent directory")?;
    fs::create_dir_all(parent)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build()?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut writer = zip::ZipWriter::new(temporary.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6))
            .unix_permissions(0o644);
        for source in &package::manifest().packages {
            println!("Preparing pinned Valve package: {}", source.name);
            let path = download(&client, source, downloads).await?;
            let mut archive = zip::ZipArchive::new(File::open(path)?)?;
            for entry in package::manifest()
                .files
                .iter()
                .filter(|entry| entry.package == source.name)
            {
                let mut file = archive.by_name(&entry.path)?;
                if file.size() != entry.size || !file.is_file() {
                    return Err(Error::VersionMismatch.into());
                }
                writer.start_file(&entry.path, options)?;
                // Bounded and hash-checked by the compiled manifest, even for
                // malformed ZIP metadata.
                package::copy_verified(
                    &mut file,
                    &mut writer,
                    entry,
                    &std::sync::atomic::AtomicBool::new(false),
                )?;
            }
        }
        writer.finish()?;
    }
    temporary.as_file().sync_all()?;
    package::verify_archive(temporary.path())?;
    temporary.persist(output)?;
    println!(
        "Verified minimal Steam runtime: {} files",
        package::manifest().files.len()
    );
    Ok(())
}
