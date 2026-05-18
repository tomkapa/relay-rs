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
};

export default vi;
