//! S3 access for the CLI (`object_store`, current-thread runtime).

use cargo_athena::S3Ref;
use std::path::Path;

use crate::{die, rt};

pub(crate) fn client(s3: &S3Ref) -> object_store::aws::AmazonS3 {
    use object_store::aws::AmazonS3Builder;
    let mut b = AmazonS3Builder::new()
        .with_bucket_name(&s3.bucket)
        .with_region(&s3.region)
        .with_allow_http(s3.insecure);
    // AWS_ENDPOINT_URL_S3, else AWS_ENDPOINT_URL (SDK precedence), overrides
    // the config endpoint (e.g. a port-forward for publish).
    let ep_env = ["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"]
        .iter()
        .find_map(|k| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    if let Some(url) = ep_env {
        if url.starts_with("http://") {
            b = b.with_allow_http(true);
        }
        b = b.with_endpoint(url);
    } else {
        // AWS proper: let region drive the URL. Custom (MinIO/etc.): use
        // the endpoint, adding a scheme if the config gave a bare host.
        let ep = s3.endpoint.trim();
        if !ep.is_empty() && !ep.ends_with("amazonaws.com") {
            let url = if ep.contains("://") {
                ep.to_string()
            } else if s3.insecure {
                format!("http://{ep}")
            } else {
                format!("https://{ep}")
            };
            b = b.with_endpoint(url);
        }
    }
    let creds = crate::s3creds::S3Credentials::new(&s3.region);
    b.with_credentials(std::sync::Arc::new(creds))
        .build()
        .unwrap_or_else(|e| die(&format!("S3 client for bucket {:?}: {e}", s3.bucket)))
}

/// Does the S3 object exist? (`cargo athena submit` pre-flights the
/// binary tarball so it doesn't submit a workflow whose pods can't
/// bootstrap.)
/// Whether the object exists. `true`/`false` ONLY distinguish present from
/// genuinely-absent (404); a real failure (403/no-creds, wrong endpoint,
/// unreachable) hard-fails with a clear message rather than masquerading
/// as "absent" — otherwise a bad credential reads as "binary not uploaded"
/// and sends you to `publish` instead of fixing the creds/endpoint.
pub(crate) fn exists(s3: &S3Ref) -> bool {
    let store = client(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    let sp = crate::feedback::spinner(format!("checking s3://{}/{}", s3.bucket, s3.key));
    let res = rt().block_on(async { object_store::ObjectStore::head(&store, &key).await });
    sp.finish_and_clear();
    match res {
        Ok(_) => true,
        Err(object_store::Error::NotFound { .. }) => false,
        Err(e) => die(&format!(
            "could not check s3://{}/{} ({e}).\n  Check the endpoint, \
             your S3 credentials (`cargo athena doctor`), and that the bucket exists.",
            s3.bucket, s3.key
        )),
    }
}

/// Delete the S3 object. Idempotent (a genuinely-absent key is fine), but
/// a REAL failure (403/no-perms, bad endpoint, network) hard-fails loudly
/// rather than being misreported as "already gone" — symmetric with the
/// WorkflowTemplate delete in `submit`.
pub(crate) fn delete(s3: &S3Ref) {
    let store = client(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    match rt().block_on(async { object_store::ObjectStore::delete(&store, &key).await }) {
        Ok(()) => {}
        // Non-S3 backends may surface a missing key as NotFound; S3/MinIO
        // DeleteObject returns 204 (Ok) for an absent key, so this arm is
        // belt-and-suspenders for idempotent re-runs.
        Err(object_store::Error::NotFound { .. }) => {}
        Err(e) => die(&format!("delete s3://{}/{}: {e}", s3.bucket, s3.key)),
    }
}

/// Stream `src` to S3 with a byte-progress bar, using multipart upload
/// for any file larger than a single part.
pub(crate) fn put(s3: &S3Ref, src: &Path) {
    use std::io::Read;

    /// 8 MB parts. S3's multipart minimum is 5 MB except for the final
    /// part; 8 keeps round trips low while still updating the bar
    /// often enough to feel live on slow uplinks.
    const PART: usize = 8 * 1024 * 1024;

    let store = client(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    let meta =
        std::fs::metadata(src).unwrap_or_else(|e| die(&format!("stat {}: {e}", src.display())));
    let total = meta.len();
    let mut file =
        std::fs::File::open(src).unwrap_or_else(|e| die(&format!("open {}: {e}", src.display())));

    rt().block_on(async {
        // Tiny files: single PUT, still show a (brief) bar.
        if total as usize <= PART {
            let mut buf = Vec::with_capacity(total as usize);
            file.read_to_end(&mut buf)
                .unwrap_or_else(|e| die(&format!("read {}: {e}", src.display())));
            let bar = crate::feedback::xfer_bar(total, "upload");
            object_store::ObjectStore::put(&store, &key, buf.into())
                .await
                .unwrap_or_else(|e| die(&format!("S3 PUT {}: {e}", s3.key)));
            bar.inc(total);
            bar.finish_and_clear();
            return;
        }

        let bar = crate::feedback::xfer_bar(total, "upload");
        let mut upload = object_store::ObjectStore::put_multipart(&store, &key)
            .await
            .unwrap_or_else(|e| die(&format!("S3 start multipart {}: {e}", s3.key)));
        let mut buf = vec![0u8; PART];
        loop {
            let n = file
                .read(&mut buf)
                .unwrap_or_else(|e| die(&format!("read {}: {e}", src.display())));
            if n == 0 {
                break;
            }
            let part: object_store::PutPayload = buf[..n].to_vec().into();
            object_store::MultipartUpload::put_part(&mut *upload, part)
                .await
                .unwrap_or_else(|e| die(&format!("S3 PUT part {}: {e}", s3.key)));
            bar.inc(n as u64);
        }
        object_store::MultipartUpload::complete(&mut *upload)
            .await
            .unwrap_or_else(|e| die(&format!("S3 complete {}: {e}", s3.key)));
        bar.finish_and_clear();
    });
}
