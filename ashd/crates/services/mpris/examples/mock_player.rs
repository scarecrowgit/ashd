//! A scriptable mock MPRIS player for the nested session (never the user's bus).
//!
//!     cargo run -p ashd-svc-mpris --example mock_player -- [--art DIR] [--bus ADDRESS]
//!
//! Serves two players on the session bus (`$DBUS_SESSION_BUS_ADDRESS`, i.e. the nested
//! session's private one): `mpd` (a track list, six tracks, starts playing track 3) and
//! `firefox` (one track, no track list, paused). With `--art DIR` a generated cover PNG
//! is written to DIR and used as `mpd`'s album art.
//!
//! Commands on stdin (one per line) drive the players; method calls the panel sends are
//! printed to stdout as `call <player> <method>`:
//!
//!     play|pause|stop <player>        set the status        (player = mpd | firefox)
//!     track <player> <n>              jump to track n (1-based)
//!     art <player> on|off             show / remove the cover
//!     jump <player> <seconds>         the player seeks by itself (`Seeked`)
//!     quit                            exit

#[path = "../tests/support/mock.rs"]
mod mock;

use std::{io::BufRead, path::PathBuf};

use mock::{MockPlayer, MockTrack};

fn cover(dir: &std::path::Path) -> std::io::Result<String> {
    // A 256x256 diagonal gradient with a grid, so scaling artefacts are visible.
    let img = image::RgbImage::from_fn(256, 256, |x, y| {
        let grid = x % 32 == 0 || y % 32 == 0;
        if grid {
            image::Rgb([240, 240, 230])
        } else {
            image::Rgb([(x as u8) / 2 + 40, (y as u8) / 2 + 20, 160u8.saturating_sub(((x + y) / 4) as u8)])
        }
    });
    std::fs::create_dir_all(dir)?;
    let path = dir.join("mock-cover.png");
    img.save(&path).map_err(std::io::Error::other)?;
    Ok(format!("file://{}", path.display()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut art_dir, mut bus): (Option<PathBuf>, Option<String>) = (None, std::env::var("DBUS_SESSION_BUS_ADDRESS").ok());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--art" => art_dir = args.next().map(PathBuf::from),
            "--bus" => bus = args.next(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let bus = bus.ok_or("no bus: set DBUS_SESSION_BUS_ADDRESS (the nested session's private bus) or pass --bus")?;
    let art_url = art_dir.as_deref().map(cover).transpose()?;

    let tracks = vec![
        MockTrack::new(1, "field-recording-01", "Hollow Pines", "Archive", 251),
        MockTrack::new(2, "field-recording-02", "Hollow Pines", "Archive", 187),
        MockTrack::new(3, "field-recording-03", "Hollow Pines", "Archive", 400),
        MockTrack::new(4, "field-recording-04", "Hollow Pines", "Archive", 252),
        MockTrack::new(5, "field-recording-05", "Hollow Pines", "Archive", 483),
        MockTrack::new(6, "a rather long track title that has to be truncated somewhere", "Hollow Pines", "Archive", 65),
    ];
    smol::block_on(async {
        let mpd = MockPlayer::start(&bus, "mpd", "mpd", tracks, true).await?;
        mpd.set_current(2).await?;
        mpd.set_status("Playing").await?;
        mpd.set_art(art_url.clone()).await?;
        let ff = MockPlayer::start(&bus, "firefox", "firefox", vec![MockTrack::new(9, "Talk: rendering on the GPU", "conf", "videos", 2710)], false).await?;
        ff.set_status("Paused").await?;
        println!("ready");

        let (tx, rx) = async_channel::unbounded::<String>();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if tx.send_blocking(line).is_err() {
                    break;
                }
            }
        });
        let mut logged = [0usize; 2];
        loop {
            // print new method calls
            for (i, p) in [&mpd, &ff].into_iter().enumerate() {
                let log = p.log();
                for l in &log[logged[i]..] {
                    println!("call {} {l}", &p.bus_name["org.mpris.MediaPlayer2.".len()..]);
                }
                logged[i] = log.len();
            }
            let line = next_line(&rx).await;
            let Some(line) = line else { continue };
            let words: Vec<&str> = line.split_whitespace().collect();
            let player = |n: &str| if n == "mpd" { &mpd } else { &ff };
            match words.as_slice() {
                ["quit"] => return Ok(()),
                ["play", p] => player(p).set_status("Playing").await?,
                ["pause", p] => player(p).set_status("Paused").await?,
                ["stop", p] => player(p).set_status("Stopped").await?,
                ["track", p, n] => player(p).set_current(n.parse::<usize>().unwrap_or(1).saturating_sub(1)).await?,
                ["art", p, "on"] => player(p).set_art(art_url.clone()).await?,
                ["art", p, "off"] => player(p).set_art(None).await?,
                ["jump", p, s] => player(p).jump_to((s.parse::<f64>().unwrap_or(0.0) * 1e6) as i64).await?,
                other => eprintln!("unknown command {other:?}"),
            }
        }
    })
}

/// A line from stdin, or `None` every 200 ms so the call log keeps being printed.
async fn next_line(rx: &async_channel::Receiver<String>) -> Option<String> {
    smol::future::or(async { rx.recv().await.ok() }, async {
        smol::Timer::after(std::time::Duration::from_millis(200)).await;
        None
    })
    .await
}
