# SodaM Privacy Policy

Effective date: October 25, 2026

SodaM is an independent desktop client for Qishui Music maintained by sodahub-org.
It is not an official product of ByteDance, Douyin, or Qishui Music. When using
Qishui Music through SodaM, the Qishui Music
[Privacy Policy](https://luna-web.douyin.com/privacy) and
[Terms of Service](https://luna-web.douyin.com/terms) also apply.

## Data We Do Not Collect

The SodaM project does not operate an advertising or user-profiling service,
and SodaM does not read data belonging to other applications. SodaM sends
necessary player diagnostics and platform-compatibility events to Qishui Music
to provide playback; their recipients and handling are described below. SodaM
does not collect files, contacts, precise location, or clipboard contents
unrelated to playback.

## Data Stored Locally

To provide sign-in and playback, SodaM stores the following on the local device:

- the Qishui Music Cookie obtained after QR-code sign-in;
- the signing-service URL and access token;
- a device identity used for request signing (`device_id`, `iid`, `fp`);
- preferences such as volume, theme, language, audio quality, shortcuts, and
  search history;
- cached audio and artwork;
- diagnostic logs.

The configuration file is stored at `%APPDATA%\sodam\config.json` on Windows,
`~/.config/sodam/config.json` on Linux, and
`~/Library/Application Support/sodam/config.json` on macOS.

The configuration contains credentials and should not be published or exchanged.
Signing out or deleting the configuration removes local sign-in information.
Clearing the cache removes cached audio and artwork.
Audio and artwork caches are written beneath a `sodam/` directory in the
system cache directory and can be redirected to another local directory with
`SODAM_CACHE_DIR`. Diagnostic logs are written beneath `sodam/logs/` in the user
data directory and can be redirected with `SODAM_LOG_DIR`. Unix systems may also
create a local socket used only to detect an already-running application instance;
it contains no account data.

## Requests to Qishui Music

After sign-in, SodaM connects directly to `api.qishui.com` and related Qishui
Music services with the user's own account in order to retrieve search results,
recommendations, playlists, lyrics, account information, playback entitlements,
and audio the user is entitled to play. These requests may contain:

- the login Cookie, account identifiers, and request identifiers issued by
  Qishui Music;
- device identifiers, client version, platform, and network type;
- search terms and requested track, playlist, or video identifiers;
- playback position, listening duration, playback source, and necessary player
  diagnostic events.

These requests are made at the user's direction. Qishui Music's processing of
this data is governed by its own privacy policy and is not controlled by the
SodaM project.

SodaM does not host a music catalog or music files. Tracks, artwork, lyrics,
and other content are provided by their rights holders and Qishui Music. SodaM
only retrieves content the user is entitled to play, claims no ownership of that
content, and does not redistribute it to other users.

## Signing Service

Some member-quality or full-length streams require app-signed request headers.
SodaM connects to a signing service provided by this project by default. Users
may instead configure their own `libmssdk` service.

To generate a signature, SodaM sends the request URL, HTTP method, and headers to
the signing service. As required by Qishui Music's request signature, those
headers may include the **Cookie**.

- The default signing service is operated by sodahub-org only to sign these
  requests;
- it does not return the Cookie to SodaM;
- the `libmssdk` application log records request shape and signature length,
  but not the Cookie, `X-Helios`, `X-Medusa`, or access token. Hosts, container
  runtimes, reverse proxies, and personnel with operational access are outside
  that application-log guarantee;
- if a user configures another signing service, that service's operator may see
  the request and is responsible for its own data practices.

The signing-service address is configurable. The built-in default currently uses
HTTP rather than HTTPS. On an untrusted network, use an HTTPS signing service or
run `libmssdk` locally. This is a statement about the actual transport, not an
additional security guarantee for the default service.

Users who do not want to send a Cookie to a remote signing service should run
`libmssdk` locally or use only features that do not require it. SodaM's
functions do not depend on selling data to third parties, and signing requests
are not used for advertising or profiling.

## Diagnostic Logs

SodaM writes diagnostic logs to local files by default to help troubleshoot playback issues. Log files are saved in the user data directory on each platform:

- Windows: `%LOCALAPPDATA%\sodam\logs\sodam.log`
- Linux: `~/.local/state/sodam/logs/sodam.log` (respecting `XDG_STATE_HOME`)
- macOS: `~/Library/Application Support/sodam/logs/sodam.log`

The log directory can also be redirected via the `SODAM_LOG_DIR` environment variable.

The logs record application startup, playback attempt stages, and necessary error diagnostics. To protect privacy, logs **never contain** sign-in Cookies, signing tokens, signature request headers, or full URL query strings (query parameters are stripped before writing to disk). The log file size is capped at 5 MB each with at most 5 rotated files kept. Because logs still contain track IDs and sanitized API paths, do not publish or publicly share log files.

## Sharing and Sale

SodaM does not sell user data or use it for advertising or profiling. Apart from
sending data to Qishui Music and its CDN/API services to provide the account,
playback, and signing functions described above, SodaM does not transmit user
data to other servers operated by the project or to unrelated third parties.

## Security and Retention

SodaM runs with ordinary user permissions. Protect the operating-system account
and configuration-file permissions, and use a trusted signing service. Local
configuration and caches remain until the user clears or deletes them. Retention
by remote services is determined by Qishui Music or the selected signing service.

## Children

SodaM does not separately collect data from children. Use of a Qishui Music
account remains subject to Qishui Music's account and age requirements.

## Changes and Contact

Updates will be published at
<https://github.com/sodahub-org/sodam/blob/main/PRIVACY.en.md>.
For privacy questions, open an issue at
<https://github.com/sodahub-org/sodam/issues>.
