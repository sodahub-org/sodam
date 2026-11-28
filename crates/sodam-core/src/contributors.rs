//! 设置页贡献者墙：contrib.rocks 组合图的生命周期状态机与拉取。

use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::cache::aged_out;
use crate::log_outlet::{log_event, LogLevel, LogTopic};

const CONTRIBUTORS_URL: &str = "https://contrib.rocks/image?repo=sodahub-org/sodam&columns=8";
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

/// 一张图算新鲜多久：拿到后满一天才允许自动重拉（#334）。
pub const CONTRIBUTORS_TTL: Duration = Duration::from_secs(86_400);
/// DNS 解析单独限时（#336）。ureq 2.12 的整体 timeout 盖不住解析——官方文档
/// 自己写明「Slow DNS resolution may cause a request to exceed the timeout,
/// because the DNS request cannot be interrupted」。所以这段必须自己兜：
/// 自定义 Resolver 把解析丢进辅助线程，主线程限时收结果。
pub const DNS_TIMEOUT: Duration = Duration::from_secs(3);
/// TCP 建连限时（#336）。ureq 默认 30 秒，太宽。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// 整个请求限时（#336）；读不单独设——`.timeout()` 会把 `timeout_read` 盖掉，
/// 一起设只剩整体这条生效，读跟着整个请求的截止时间走。
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 一轮最多自动试几次；用完停手，只等用户点重试或图过期重开（#336）。
pub const MAX_ATTEMPTS: u8 = 3;
/// 第 1、2 次失败之后分别等多久再自动试。表长绑死为 `MAX_ATTEMPTS - 1`：
/// 改次数不改这张表，编译先不过（#339 Testing Decisions 的变异验收）。
pub const RETRY_DELAYS: [Duration; (MAX_ATTEMPTS - 1) as usize] =
    [Duration::from_secs(10), Duration::from_secs(30)];

/// 贡献者墙的全部状态：图、在飞、这一轮的账、下次允许自动再试的时刻。
///
/// 「该不该拉」只有这一个判定来源（#339 实现决策 1）。不碰网络、不碰磁盘、
/// 不碰 GPUI，时间一律由调用方传入，单测可以穷举重绘风暴。
/// `Img` 是图本体，应用层放 `Arc<gpui::Image>`。
pub struct ContributorsState<Img> {
    image: Option<(Img, SystemTime)>,
    loading: bool,
    attempts: u8,
    next_try_at: Option<SystemTime>,
}

impl<Img> Default for ContributorsState<Img> {
    fn default() -> Self {
        Self {
            image: None,
            loading: false,
            attempts: 0,
            next_try_at: None,
        }
    }
}

impl<Img> ContributorsState<Img> {
    /// 当前要显示的图（有的话）。
    pub fn image(&self) -> Option<&Img> {
        self.image.as_ref().map(|(image, _)| image)
    }

    /// 现在该不该拉，优先级从高到低：在飞 → 人点重试 → 三次用完 → 退避期 → 图的新鲜度。
    ///
    /// * 在飞时连 `force` 也不放行：点重试点得再快也不会叠出第二个请求；
    /// * `force` 是用户点「重试」，越过停手与新鲜图，`begin(true)` 给它重开一整轮；
    /// * 有截止时间时「到点即拉」：没到就等，热失败冷却一天也走这一条；
    /// * 没图就该拉；有图满 [`CONTRIBUTORS_TTL`] 才拉，时钟回拨按还没过期算。
    ///
    /// 设置页每帧问一次，所以「三次用完就停手」是反死循环的关键
    /// （#135 C：失败 → `cx.notify()` → 重绘 → 再拉取的循环就靠它断掉）。
    pub fn should_fetch(&self, now: SystemTime, force: bool) -> bool {
        if self.loading {
            return false;
        }
        if force {
            return true;
        }
        if self.attempts >= MAX_ATTEMPTS {
            return false;
        }
        if let Some(next_try_at) = self.next_try_at {
            return now >= next_try_at;
        }
        match self.image.as_ref() {
            None => true,
            Some((_, image_at)) => aged_out(*image_at, CONTRIBUTORS_TTL, now),
        }
    }

    /// 开跑一次拉取：置在飞。`force`（用户点重试）额外重开一整轮——
    /// 账清零、截止时间清空，点一次拿到的是完整的三次机会，不是第 4 次。
    pub fn begin(&mut self, force: bool) {
        self.loading = true;
        if force {
            self.attempts = 0;
            self.next_try_at = None;
        }
    }

    /// 手上有图（缓存命中）先挂出来；不动在飞标记，这一轮可能还要接着问网络。
    pub fn show_cached(&mut self, image: Img, image_at: SystemTime) {
        self.image = Some((image, image_at));
    }

    /// 拉取成功：换图，账擦干净。截止时间必须一起清，否则旧截止时间一到，
    /// 判定无视新鲜图直接放行，成功路径自己变成新的死循环。
    pub fn finish_ok(&mut self, image: Img, image_at: SystemTime) {
        self.image = Some((image, image_at));
        self.loading = false;
        self.attempts = 0;
        self.next_try_at = None;
    }

    /// 拉取失败：记一笔账，绝不碰图——有旧图时用户看到的还是上一张。
    ///
    /// 账没用完：按第几次失败等 [`RETRY_DELAYS`] 里对应的时长再自动试；
    /// 用完三次：有图（热失败）把账归零、等一天重开一轮，没图（冷失败）
    /// 停手，只等用户点重试。
    pub fn finish_err(&mut self, now: SystemTime) {
        self.loading = false;
        // 账最多记到停手为止，正常路径到 3 就封顶，这里不该 panic。
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts < MAX_ATTEMPTS {
            self.next_try_at = Some(now + RETRY_DELAYS[(self.attempts - 1) as usize]);
        } else if self.image.is_some() {
            // 热失败：图还在，等一天重开一轮；账归零，否则下一轮一开
            // 就被「三次用完」挡住，永远轮不到。
            self.attempts = 0;
            self.next_try_at = Some(now + CONTRIBUTORS_TTL);
        } else {
            // 冷失败：停手等人，没有自动重试了。
            self.next_try_at = None;
        }
    }

    /// 界面要不要亮出「重试」：没图、且这一轮真的失败过。
    /// 有旧图时失败完全不打扰（热失败）；点完重试账清零，这句话立刻收起。
    pub fn shows_retry(&self) -> bool {
        self.image.is_none() && self.attempts > 0
    }
}

/// SVG 门槛：只看开头还不够，截断的文档一律当没图，必须有 `</svg>` 收尾。
fn is_svg(bytes: &[u8]) -> bool {
    let body = bytes.trim_ascii();
    (body.starts_with(b"<svg") || body.starts_with(b"<?xml")) && body.ends_with(b"</svg>")
}

/// 把 [`DNS_TIMEOUT`] 兜进拉图请求：解析这段 ureq 管不了，只能包成
/// [`ureq::Resolver`] 自己限时。
struct DeadlineResolver;

impl ureq::Resolver for DeadlineResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let netloc = netloc.to_owned();
        spawn_with_timeout(
            move || {
                netloc
                    .to_socket_addrs()
                    .map(|addrs| addrs.collect::<Vec<SocketAddr>>())
            },
            DNS_TIMEOUT,
        )
    }
}

/// 把一段可能卡死的网络工作丢进辅助线程，主线程只等 `timeout`：到点就报
/// TimedOut，不等它自己回来（`ToSocketAddrs` 没有可中断的 API）。
/// 超时后辅助线程还没干完也没人收了，随它自然结束，不影响下一次请求。
fn spawn_with_timeout<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    timeout: Duration,
) -> std::io::Result<T> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            Err(std::io::Error::new(ErrorKind::TimedOut, "DNS 解析超时"))
        }
        // 解析线程自己没了（panic / 提前退出），结果永远等不来，按出错收。
        Err(RecvTimeoutError::Disconnected) => Err(std::io::Error::new(
            ErrorKind::Interrupted,
            "解析线程中途退出",
        )),
    }
}

/// 读门槛（#343）：「最多 4 MiB」，所以多读一字节才能分清「正好读满」和
/// 「读满还没结束」；后者是截断，返回 None，调用方不会把截断的图写进缓存。
/// 读出错与截断各记一条 Warn，带上已读字节数，失败不再是无声的 None。
fn read_capped(reader: impl Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Err(err) = reader.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes) {
        let error = err.to_string();
        let read_bytes = bytes.len().to_string();
        log_event(
            LogLevel::Warn,
            LogTopic::App,
            "contributors_read_failed",
            &[
                ("error", error.as_str()),
                ("read_bytes", read_bytes.as_str()),
            ],
        );
        return None;
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        // take 把上限卡死了：走到这一支说明读满还没结束，读到的必定是
        // 上限 + 1 字节，读多少报不出来，所以直接报上限本身。
        let cap_bytes = MAX_IMAGE_BYTES.to_string();
        log_event(
            LogLevel::Warn,
            LogTopic::App,
            "contributors_read_over_cap",
            &[("cap_bytes", cap_bytes.as_str())],
        );
        return None;
    }
    Some(bytes)
}

/// 下载 contrib.rocks 动态生成的 SVG；返回原始矢量字节。
///
/// 三段超时各自生效（#336）：DNS [`DNS_TIMEOUT`]（解析自己兜，见
/// [`DeadlineResolver`]）、TCP 建连 [`CONNECT_TIMEOUT`]、整体
/// [`TOTAL_TIMEOUT`]；读不单独设，响应体跟着整体截止时间走。
/// 四种失败各记一条 Warn：请求失败、读出错、超限、拿到的东西不是 SVG
/// （服务端给错误页、提前断流最常见的坏法），退避节奏下不会刷屏。
pub fn fetch_contributors_svg() -> Option<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .resolver(DeadlineResolver)
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .build();
    let response = match agent.get(CONTRIBUTORS_URL).call() {
        Ok(response) => response,
        Err(err) => {
            let error = err.to_string();
            log_event(
                LogLevel::Warn,
                LogTopic::App,
                "contributors_fetch_failed",
                &[("error", error.as_str())],
            );
            return None;
        }
    };
    // content-type 必须在 into_reader 之前抓：那一步会把响应体消费掉。
    // text/html 多半是错误页，image/svg+xml 才是断了流的图。
    let content_type = response
        .header("content-type")
        .unwrap_or("unknown")
        .to_owned();
    let bytes = read_capped(response.into_reader())?;
    if !is_svg(&bytes) {
        let read_bytes = bytes.len().to_string();
        log_event(
            LogLevel::Warn,
            LogTopic::App,
            "contributors_body_rejected",
            &[
                ("content_type", content_type.as_str()),
                ("read_bytes", read_bytes.as_str()),
            ],
        );
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const IMG: u8 = 7;
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// 测试用空状态；`Img` 用 u8 占位，`default()` 不带图时推不出图类型。
    fn empty() -> ContributorsState<u8> {
        ContributorsState::default()
    }

    /// 完整 SVG 收，截断的不收，空的不收，不是 SVG 的不收。
    #[test]
    fn accepts_complete_svg_documents_only() {
        assert!(is_svg(
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"
        ));
        assert!(is_svg(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"));
        // 有开头没收尾：截断。
        assert!(!is_svg(
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\""
        ));
        assert!(!is_svg(b""));
        assert!(!is_svg(b"<html>not an image</html>"));
    }

    /// 三段超时钉死数值（#336 / #339 Testing Decisions：每个数值只有一处定义）。
    #[test]
    fn three_timeouts_are_pinned() {
        assert_eq!(DNS_TIMEOUT, Duration::from_secs(3));
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(TOTAL_TIMEOUT, Duration::from_secs(10));
    }

    /// 解析限时真的限时：活儿睡一秒，期限 10 毫秒——到点报 TimedOut 回来，
    /// 不干等系统解析自己结束（DNS 卡住时 3 秒内失败的那条保障）。
    #[test]
    fn spawn_with_timeout_cuts_off_a_hanging_resolution() {
        let started = Instant::now();
        let result = spawn_with_timeout(
            || {
                thread::sleep(Duration::from_secs(1));
                Ok(Vec::<u8>::new())
            },
            Duration::from_millis(10),
        );
        let err = result.expect_err("超期的解析必须按失败收");
        assert_eq!(err.kind(), ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "到点就回来，不等活儿干完"
        );
    }

    /// 解析正常完成时结果原样带回，限时机制不碰成功的路。
    #[test]
    fn spawn_with_timeout_passes_through_a_finished_resolution() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 443));
        let result = spawn_with_timeout(move || Ok(vec![addr]), Duration::from_secs(1));
        assert_eq!(result.expect("正常完成不该报错"), vec![addr]);
    }

    /// 截断判据（#343）：正好读满 4 MiB 收；多一个字节（读满还没结束）拒。
    /// `>` 写成 `>=`、`+1` 漏掉，这里都会红。
    #[test]
    fn read_capped_enforces_the_four_mib_cap() {
        let at_cap = read_capped(std::io::repeat(b'a').take(MAX_IMAGE_BYTES)).unwrap();
        assert_eq!(at_cap.len() as u64, MAX_IMAGE_BYTES);
        assert!(read_capped(std::io::repeat(b'a').take(MAX_IMAGE_BYTES + 1)).is_none());
    }

    /// 短于门槛的响应体原样带回：截断判据不碰 4 MiB 以内的内容，收不收由
    /// is_svg 的收尾门槛说话（截断的 SVG 被拒由它自己的测试承载）。
    #[test]
    fn read_capped_passes_shorter_bodies_through() {
        let body: &[u8] =
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(read_capped(body).unwrap(), body);
    }

    /// 首次进设置页：没图、没账 —— 该拉一次；开跑之后同一帧再问就不拉。
    #[test]
    fn first_visit_fetches_once() {
        let mut state = empty();
        assert!(state.should_fetch(at(0), false));
        state.begin(false);
        assert!(!state.should_fetch(at(0), false));
    }

    /// 图没满一天：再多次重绘也不重拉。
    #[test]
    fn fresh_image_stays_idle() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        assert!(!state.should_fetch(at(1), false));
        assert!(!state.should_fetch(at(23 * HOUR), false));
        assert!(!state.should_fetch(at(DAY - 1), false));
    }

    /// 图满 24 小时（含整点）就该重拉；超过更是。
    #[test]
    fn expired_image_refetches() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        assert!(state.should_fetch(at(DAY), false));
        assert!(state.should_fetch(at(30 * DAY), false));
    }

    /// 在飞时不重拉：渲染路径的自动询问和用户点重试都不叠第二个请求。
    #[test]
    fn in_flight_never_refetches() {
        let mut state = empty();
        state.begin(false);
        assert!(!state.should_fetch(at(0), false));
        assert!(!state.should_fetch(at(0), true));
    }

    /// 第 1 次失败后进入 10 秒退避：9 秒不拉，10 秒整到点就拉。
    #[test]
    fn failure_waits_ten_seconds() {
        let mut state = empty();
        state.begin(false);
        state.finish_err(at(0));
        assert!(!state.should_fetch(at(9), false));
        assert!(state.should_fetch(at(10), false));
    }

    /// 第 2 次失败后进入 30 秒退避：29 秒不拉，30 秒整到点就拉。
    #[test]
    fn second_failure_waits_thirty_seconds() {
        let mut state = empty();
        state.begin(false);
        state.finish_err(at(0));
        state.begin(false);
        state.finish_err(at(10));
        assert!(!state.should_fetch(at(39), false));
        assert!(state.should_fetch(at(40), false));
    }

    /// force 越过新鲜图：用户点重试，图再新也放行。
    #[test]
    fn force_overrides_fresh_image() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        assert!(state.should_fetch(at(1), true));
    }

    /// 三次用完停手之后，只有 force 能重开；点完是新一轮三次，不是第 4 次。
    #[test]
    fn force_starts_a_new_round_from_exhausted() {
        let mut state = empty();
        for t in [0, 10, 40] {
            state.begin(false);
            state.finish_err(at(t));
        }
        assert!(!state.should_fetch(at(40), false));
        assert!(state.should_fetch(at(40), true));
        state.begin(true);
        assert_eq!(state.attempts, 0);
        assert_eq!(state.next_try_at, None);
        // 新一轮的第 1 次失败照样按表等 10 秒，而不是被旧账撑成停手。
        state.finish_err(at(100));
        assert_eq!(state.next_try_at, Some(at(110)));
        assert!(state.should_fetch(at(110), false));
    }

    /// 成功把账擦干净：旧截止时间到了也不再放行，成功路径不变成死循环。
    #[test]
    fn success_clears_the_deadline() {
        let mut state = empty();
        state.begin(false);
        state.finish_err(at(0));
        state.begin(false);
        state.finish_ok(IMG, at(10));
        assert_eq!(state.attempts, 0);
        assert_eq!(state.next_try_at, None);
        assert!(!state.should_fetch(at(10), false));
        assert!(!state.should_fetch(at(11), false));
    }

    /// 冷失败三次用完停手：一年之后也不自动拉，只等用户点重试。
    #[test]
    fn cold_round_exhausted_stops_for_good() {
        let mut state = empty();
        for t in [0, 10, 40] {
            state.begin(false);
            state.finish_err(at(t));
        }
        assert!(!state.should_fetch(at(3_600), false));
        assert!(!state.should_fetch(at(365 * DAY), false));
    }

    /// 热失败三次用完：图还在，退避一天后重开一轮；冷却期内就算图已过期也不拉。
    #[test]
    fn hot_round_exhausted_waits_a_day() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        // 图放了一天才到期自动重拉，连败三次。
        assert!(state.should_fetch(at(25 * DAY), false));
        for extra in [0, 10, 40] {
            state.begin(false);
            state.finish_err(at(25 * DAY + extra));
        }
        assert!(state.image.is_some());
        assert!(!state.should_fetch(at(25 * DAY + 40 + 3_600), false));
        assert!(!state.should_fetch(at(25 * DAY + 40 + 23 * HOUR), false));
        assert!(state.should_fetch(at(25 * DAY + 40 + DAY), false));
    }

    /// 热失败不抹图、也不亮重试文案：有旧图时失败一个字都不写。
    #[test]
    fn hot_failure_keeps_the_image() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        state.begin(false);
        state.finish_err(at(1));
        assert_eq!(state.image.as_ref().map(|(image, _)| *image), Some(IMG));
        assert!(!state.shows_retry());
    }

    /// 重试文案只在「没图 + 这一轮失败过」出现；点完立刻收起；热失败不出现。
    #[test]
    fn retry_prompt_needs_a_failure_and_no_image() {
        let mut state = empty();
        assert!(!state.shows_retry());
        state.begin(false);
        state.finish_err(at(0));
        assert!(state.shows_retry());
        // 用户点了重试：账清零，这句话先收起。
        state.begin(true);
        assert!(!state.shows_retry());
        // 这次重试成功了；之后过期重拉再失败：有图，照样不亮。
        state.finish_ok(IMG, at(5));
        state.begin(false);
        state.finish_err(at(6));
        assert!(!state.shows_retry());
        // 热失败之后用户点重试：begin(true) 连旧截止时间一起清掉。
        state.begin(true);
        assert!(!state.shows_retry());
        assert_eq!(state.next_try_at, None);
    }

    /// 一小时按 1 毫秒重绘（360 万次询问），冷路径恰好 3 次请求就停手。
    #[test]
    fn redraw_storm_cold_fetch_count_is_exactly_three() {
        let mut state = empty();
        let mut fetches = 0;
        for ms in 0..(HOUR * 1_000) {
            let now = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
            if state.should_fetch(now, false) {
                fetches += 1;
                state.begin(false);
                state.finish_err(now);
            }
        }
        assert_eq!(fetches, 3);
    }

    /// 两天按 1 秒重绘：图常在（热路径），每 24 小时恰好重试 3 次。
    #[test]
    fn redraw_storm_hot_fetch_count_is_exactly_three_per_day() {
        let mut state = empty();
        state.show_cached(IMG, at(0));
        let mut fetches = 0;
        let mut fetched_in_first_day = 0;
        for s in 0..(2 * DAY + 120) {
            let now = at(s);
            if state.should_fetch(now, false) {
                fetches += 1;
                if s < DAY {
                    fetched_in_first_day += 1;
                }
                state.begin(false);
                state.finish_err(now);
            }
        }
        // 到期前一次都不拉；之后每 24 小时一轮，一轮恰好 3 次。
        assert_eq!(fetched_in_first_day, 0);
        assert_eq!(fetches, 6);
    }
}
