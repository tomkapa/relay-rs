// Vietnamese string table. Mirrors `en.ts` key-for-key.
//
// Translation notes:
// - Brand "Relay" stays English (product name).
// - "Sign in" → "Đăng nhập"; "Channels" → "Kênh"; "Direct Messages" →
//   "Tin nhắn riêng"; "Sign out" → "Đăng xuất"; "Language" → "Ngôn ngữ".

import type { TranslationTable } from "./en";

const vi: TranslationTable = {
  "signin.brand": "Relay",
  "signin.tagline": "Bảng điều khiển vận hành cho các phiên đa agent.",
  "signin.heading": "Đăng nhập vào Relay",
  "signin.subheading": "Tiếp tục bằng tài khoản Google của bạn.",
  "signin.cta": "Tiếp tục bằng Google",
  "signin.legal": "Khi tiếp tục, bạn đồng ý với Điều khoản dịch vụ và Chính sách bảo mật.",
  "signin.error.forbidden": "Truy cập bị từ chối.",
  "signin.error.oauth_down": "Hiện không thể đăng nhập. Vui lòng thử lại sau.",

  "sidebar.brand": "Relay",
  "sidebar.channels": "Kênh",
  "sidebar.dms": "Tin nhắn riêng",
  "sidebar.empty_agents": "Chưa có agent nào được đăng ký.",

  "usermenu.signout": "Đăng xuất",
  "usermenu.language.label": "Ngôn ngữ",
  "usermenu.language.en": "English",
  "usermenu.language.vi": "Tiếng Việt",
  "usermenu.language.error": "Không thể đổi ngôn ngữ. Vui lòng thử lại.",

  "button.loading": "Đang tải",

  // ─── Connections ─────────────────────────────────────────────────
  "menu.connections": "Kết nối",
  "connections.workspace.label": "WORKSPACE ACME",
  "connections.nav.title": "Kết nối",
  "connections.nav.browse": "Duyệt danh mục",
  "connections.nav.my": "Kết nối của tôi",
  "connections.breadcrumb.workspace": "Workspace",
  "connections.breadcrumb.connections": "Kết nối",
  "connections.breadcrumb.my": "Kết nối của tôi",
  "connections.breadcrumb.add": "Thêm kết nối",
  "connections.list.title": "Kết nối của tôi",
  "connections.list.subtitle":
    "Các công cụ đã kết nối sẵn có cho workspace. Admin kết nối — chủ agent cho phép.",
  "connections.list.add": "Thêm kết nối",
  "connections.list.filter": "Tất cả trạng thái",
  "connections.list.empty.title": "Chưa có kết nối nào",
  "connections.list.empty.body":
    "Duyệt danh mục để kết nối Notion, Linear, GitHub và các dịch vụ khác.",
  "connections.list.empty.cta": "Duyệt danh mục",
  "connections.list.col.status": "TRẠNG THÁI",
  "connections.list.col.connection": "KẾT NỐI",
  "connections.list.col.owner": "CHỦ SỞ HỮU",
  "connections.list.col.tools": "CÔNG CỤ",
  "connections.list.col.lastSeen": "HOẠT ĐỘNG GẦN NHẤT",
  "connections.list.col.enable": "BẬT",
  "connections.status.ok": "Tốt",
  "connections.status.reconnect": "Kết nối lại",
  "connections.status.error": "Lỗi",
  "connections.status.pending": "Chờ",
  "connections.row.reconnect": "Kết nối lại",
  "connections.row.disconnect": "Ngắt kết nối",
  "connections.row.remove": "Xóa",
  "connections.row.never": "chưa từng",
  "connections.row.toolsSuffix": "công cụ",
  "connections.catalog.title": "Thêm kết nối",
  "connections.catalog.subtitle":
    "Kết nối Notion, Linear, GitHub và các dịch vụ khác để agent có thể thao tác trên dữ liệu thật.",
  "connections.catalog.search": "Tìm nhà cung cấp",
  "connections.catalog.tabs.all": "Tất cả",
  "connections.catalog.tabs.productivity": "Năng suất",
  "connections.catalog.tabs.dev": "Công cụ lập trình",
  "connections.catalog.tabs.comms": "Liên lạc",
  "connections.catalog.tabs.data": "Dữ liệu",
  "connections.catalog.tabs.custom": "Tùy chỉnh",
  "connections.catalog.sort": "Sắp xếp:",
  "connections.catalog.sort.mostUsed": "Dùng nhiều nhất",
  "connections.catalog.tool": "công cụ",
  "connections.catalog.tools": "công cụ",
  "connections.catalog.added": "Đã thêm",
  "connections.catalog.custom.title": "+ Tùy chỉnh",
  "connections.catalog.custom.blurb": "Dán URL của máy chủ MCP.",
  "connections.catalog.empty": "Không có nhà cung cấp nào khớp.",
  "connections.modal.cancel": "Hủy",
  "connections.modal.close": "Đóng",
  "connections.modal.oauth.eyebrow": "Kết nối",
  "connections.modal.oauth.title": "Kết nối {name} với workspace",
  "connections.modal.oauth.bullet1":
    "Sẵn sàng cho agent trong workspace khi admin bật trên agent.",
  "connections.modal.oauth.bullet2": "Bạn có thể ngắt kết nối bất cứ lúc nào.",
  "connections.modal.oauth.bullet3":
    "{name} sẽ hỏi bạn chọn trang và cơ sở dữ liệu nào để chia sẻ.",
  "connections.modal.oauth.continue": "Tiếp tục đến {name}",
  "connections.modal.token.tokenLabel": "API token cho {name}",
  "connections.modal.token.help": "Tìm token này ở đâu?",
  "connections.modal.token.placeholder": "Dán API token của {name}",
  "connections.modal.token.note":
    "Lưu mã hóa. Sau khi lưu sẽ không hiển thị lại.",
  "connections.modal.token.connect": "Kết nối",
  "connections.modal.token.testing": "Đang kiểm tra kết nối…",
  "connections.modal.token.error":
    "Không thể kết nối tới {name}. Kiểm tra token và thử lại.",
  "connections.modal.reconnect.eyebrow": "Kết nối lại",
  "connections.modal.reconnect.title": "Kết nối lại {name}",
  "connections.modal.reconnect.alertTitle": "Quyền của {name} đã hết hạn",
  "connections.modal.reconnect.alertBody":
    "Làm mới token thất bại. Kết nối lại để khôi phục quyền truy cập.",
  "connections.modal.reconnect.lastSeen": "Cuộc gọi thành công gần nhất",
  "connections.modal.reconnect.upstream": "Phản hồi từ máy chủ",
  "connections.modal.reconnect.notNow": "Để sau",
  "connections.modal.reconnect.cta": "Kết nối lại {name}",
  "connections.modal.custom.eyebrow": "Admin · Máy chủ tùy chỉnh",
  "connections.modal.custom.title": "Thêm máy chủ tùy chỉnh",
  "connections.modal.custom.nameLabel": "Tên hiển thị",
  "connections.modal.custom.namePlaceholder": "internal-search",
  "connections.modal.custom.urlLabel": "URL máy chủ MCP",
  "connections.modal.custom.urlHint":
    "http:// sẽ bị từ chối. URL phải truy cập được từ Relay worker.",
  "connections.modal.custom.authLabel": "Xác thực",
  "connections.modal.custom.authNone": "Không",
  "connections.modal.custom.authToken": "API token",
  "connections.modal.custom.warn":
    "Mọi agent admin bật sẽ có thể gọi URL này mà không cần thông tin xác thực. Chỉ dùng cho máy chủ nội bộ tin cậy.",
  "connections.modal.custom.cta": "Thêm máy chủ",
  "connections.modal.custom.error.alias":
    "Dùng chữ thường, chữ số, _ hoặc -, tối đa 16 ký tự.",
  "connections.modal.custom.error.url": "Nhập URL https:// hợp lệ.",
  "connections.callback.eyebrow.connecting": "OAuth · Bước 2 / 3",
  "connections.callback.eyebrow.authorized": "OAuth · Bước 3 / 3",
  "connections.callback.eyebrow.failed": "OAuth · Thất bại",
  "connections.callback.connecting.title": "Đang xin {name} chia sẻ quyền truy cập…",
  "connections.callback.connecting.body":
    "Màn hình đồng ý của {name} sẽ mở trong một tab mới.",
  "connections.callback.authorized.title": "Đã kết nối {name}.",
  "connections.callback.authorized.discovering": "Đang khám phá công cụ…",
  "connections.callback.authorized.body":
    "Công cụ sẽ tự làm mới. Đã khám phá {count} công cụ.",
  "connections.callback.authorized.redirect": "Chuyển trang sau {seconds}s.",
  "connections.callback.authorized.goNow": "Đi tới kết nối ngay →",
  "connections.callback.failed.title": "Kết nối thất bại",
  "connections.callback.failed.bodyDenied":
    "{name} không trả về token. Không có gì được thêm.",
  "connections.callback.failed.bodyGeneric":
    "Không hoàn tất được bắt tay OAuth. Không có gì được thêm.",
  "connections.callback.failed.options": "Tùy chọn",
  "connections.callback.failed.response": "Phản hồi",
  "connections.callback.failed.reference": "Mã tham chiếu",
  "connections.callback.failed.back": "Quay lại danh mục",
  "connections.callback.failed.retry": "Thử lại",
  "connections.callback.steps.redirected": "Đã chuyển hướng",
  "connections.callback.steps.awaiting": "Đang chờ đồng ý",
  "connections.callback.steps.discover": "Khám phá công cụ",
  "connections.confirm.removeTitle": "Xóa kết nối này?",
  "connections.confirm.removeBody":
    "Agent đang phụ thuộc sẽ mất quyền truy cập ngay lập tức.",
  "connections.confirm.disconnectTitle": "Ngắt thông tin xác thực?",
  "connections.confirm.disconnectBody":
    "Máy chủ vẫn ở trong danh sách nhưng agent không thể gọi tới đến khi bạn kết nối lại.",
  "connections.confirm.cancel": "Hủy",
  "connections.confirm.confirm": "Xác nhận",
};

export default vi;
