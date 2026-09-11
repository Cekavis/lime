#include "weasel_ui_adapter.h"

#include <windows.h>

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>
#include <unordered_map>
#include <utility>

namespace lime::tsf {
namespace {

using ThemeMap = std::unordered_map<std::string, std::string>;

void InjectVirtualKey(WORD key) {
  INPUT inputs[2]{};
  inputs[0].type = INPUT_KEYBOARD;
  inputs[0].ki.wVk = key;
  // PageUp/PageDown and the arrow keys are extended keys.  Marking them as
  // such is important when the event is synthesized from the UI thread:
  // without the flag some hosts interpret VK_NEXT/VK_PRIOR as the numeric
  // keypad keys instead of forwarding the navigation intent to TSF.
  const bool extended = key == VK_PRIOR || key == VK_NEXT || key == VK_UP ||
                        key == VK_DOWN || key == VK_LEFT || key == VK_RIGHT;
  if (extended) inputs[0].ki.dwFlags = KEYEVENTF_EXTENDEDKEY;
  inputs[1] = inputs[0];
  inputs[1].ki.dwFlags = KEYEVENTF_KEYUP |
                         (extended ? KEYEVENTF_EXTENDEDKEY : 0);
  SendInput(ARRAYSIZE(inputs), inputs, sizeof(INPUT));
}

struct ThemeDocument {
  ThemeMap style;
  ThemeMap layout;
  std::unordered_map<std::string, ThemeMap> schemes;
};

struct PatchFrame {
  int indent = -1;
  std::string path;
};

std::string Trim(std::string value) {
  const auto first = value.find_first_not_of(" \t\r\n");
  if (first == std::string::npos) return {};
  const auto last = value.find_last_not_of(" \t\r\n");
  return value.substr(first, last - first + 1);
}

std::string StripComment(std::string value) {
  bool quoted = false;
  char quote = 0;
  for (size_t i = 0; i < value.size(); ++i) {
    const char c = value[i];
    if ((c == '\'' || c == '"')) {
      if (!quoted) {
        quoted = true;
        quote = c;
      } else if (quote == c && (i == 0 || value[i - 1] != '\\')) {
        quoted = false;
      }
    } else if (c == '#' && !quoted && (i == 0 || value[i - 1] == ' ' || value[i - 1] == '\t')) {
      value.resize(i);
      break;
    }
  }
  return Trim(std::move(value));
}

std::string Unquote(std::string value) {
  value = Trim(std::move(value));
  if (value.size() >= 2 &&
      ((value.front() == '"' && value.back() == '"') ||
       (value.front() == '\'' && value.back() == '\''))) {
    value = value.substr(1, value.size() - 2);
  }
  return value;
}

bool ParseBool(const ThemeMap& map, std::string_view key, bool& out) {
  const auto it = map.find(std::string(key));
  if (it == map.end()) return false;
  const std::string value = Unquote(it->second);
  if (value == "true" || value == "yes" || value == "on") {
    out = true;
    return true;
  }
  if (value == "false" || value == "no" || value == "off") {
    out = false;
    return true;
  }
  return false;
}

bool ParseInt(const ThemeMap& map, std::string_view key, int& out) {
  const auto it = map.find(std::string(key));
  if (it == map.end()) return false;
  std::string value = Unquote(it->second);
  try {
    size_t used = 0;
    const long parsed = std::stol(value, &used, 0);
    if (used != value.size()) return false;
    out = static_cast<int>(parsed);
    return true;
  } catch (...) {
    return false;
  }
}

bool ParseString(const ThemeMap& map, std::string_view key, std::string& out) {
  const auto it = map.find(std::string(key));
  if (it == map.end()) return false;
  out = Unquote(it->second);
  return true;
}

bool ParseColorValue(const std::string& input, uint32_t& out) {
  std::string value = Unquote(input);
  if (value.size() > 1 && value[0] == '#') {
    value.erase(0, 1);
  } else if (value.size() > 2 && value[0] == '0' &&
             (value[1] == 'x' || value[1] == 'X')) {
    value.erase(0, 2);
  } else {
    // Rime may expose a color as an integer scalar rather than a quoted
    // 0x-prefixed string.  Accept decimal values as the upstream parser does.
    try {
      size_t used = 0;
      const unsigned long parsed = std::stoul(value, &used, 10);
      if (used == value.size() && parsed <= 0xfffffffful) {
        out = static_cast<uint32_t>(parsed);
        return true;
      }
    } catch (...) {
    }
    return false;
  }
  if (value.size() != 3 && value.size() != 4 && value.size() != 6 &&
      value.size() != 8) return false;
  if (value.size() == 3 || value.size() == 4) {
    std::string expanded;
    expanded.reserve(value.size() * 2);
    for (const char c : value) {
      expanded.push_back(c);
      expanded.push_back(c);
    }
    value = std::move(expanded);
  }
  try {
    size_t used = 0;
    out = static_cast<uint32_t>(std::stoul(value, &used, 16));
    return used == value.size();
  } catch (...) {
    return false;
  }
}

int ToAbgr(uint32_t value, std::string format) {
  format = Unquote(std::move(format));
  if (value <= 0x00ffffffu) {
    value = format == "rgba" ? (value << 8) | 0xffu : value | 0xff000000u;
  }
  if (format == "argb") {
    value = (value & 0xff000000u) | ((value & 0x000000ffu) << 16) |
            (value & 0x0000ff00u) | ((value & 0x00ff0000u) >> 16);
  } else if (format == "rgba") {
    value = ((value & 0x000000ffu) << 24) | ((value & 0xff000000u) >> 24) |
            ((value & 0x00ff0000u) >> 8) | ((value & 0x0000ff00u) << 8);
  }
  return static_cast<int>(value);
}

int Color(uint8_t r, uint8_t g, uint8_t b, uint8_t a = 0xff) {
  return static_cast<int>((static_cast<uint32_t>(a) << 24) |
                          (static_cast<uint32_t>(b) << 16) |
                          (static_cast<uint32_t>(g) << 8) | r);
}

std::string NormalizePatchPath(std::string path) {
  path = Unquote(Trim(std::move(path)));
  std::string normalized;
  size_t start = 0;
  while (start <= path.size()) {
    const size_t slash = path.find('/', start);
    std::string component =
        Trim(path.substr(start, slash == std::string::npos ? std::string::npos
                                                           : slash - start));
    if (!component.empty()) {
      if (component == "+") {
        // Rime's patch merge operator (for example, style/+).  The
        // containing map is the target; it is not part of the key.
      } else {
        if (!normalized.empty()) normalized.push_back('/');
        normalized += component;
      }
    }
    if (slash == std::string::npos) break;
    start = slash + 1;
  }
  return normalized;
}

void ApplyPatchValue(ThemeDocument& document, std::string path,
                     const std::string& value) {
  path = NormalizePatchPath(std::move(path));
  if (path.empty()) return;

  std::vector<std::string> components;
  size_t start = 0;
  while (start <= path.size()) {
    const size_t slash = path.find('/', start);
    components.push_back(path.substr(
        start, slash == std::string::npos ? std::string::npos : slash - start));
    if (slash == std::string::npos) break;
    start = slash + 1;
  }
  if (components.empty()) return;

  if (components[0] == "style") {
    if (components.size() == 2) {
      document.style[components[1]] = value;
    } else if (components.size() >= 3 && components[1] == "layout") {
      document.layout[components.back()] = value;
    }
    return;
  }
  // Some existing Weasel custom files use layout/... as a shorthand for
  // style/layout/....
  if (components[0] == "layout" && components.size() >= 2) {
    document.layout[components.back()] = value;
    return;
  }
  if (components[0] == "preset_color_schemes" && components.size() >= 3) {
    document.schemes[components[1]][components.back()] = value;
  }
}

void SetDefaultStyle(weasel::UIStyle& style) {
  style.font_face = L"Segoe UI, Microsoft YaHei, DengXian";
  style.label_font_face = L"Microsoft YaHei";
  style.comment_font_face = L"Microsoft YaHei";
  style.font_point = 14;
  style.label_font_point = 14;
  style.comment_font_point = 13;
  style.candidate_abbreviate_length = 30;
  // Lime's TSF composition is rendered by the host editor.  Advertise the
  // inline-preedit capability so WeaselUI keeps the host-owned composition out
  // of the popup layout; the preceding-text sidecar remains the auxiliary row.
  style.inline_preedit = true;
  style.client_caps = weasel::INLINE_PREEDIT_CAPABLE;
  style.paging_on_scroll = true;
  style.enhanced_position = true;
  style.layout_type = weasel::UIStyle::LAYOUT_VERTICAL;
  style.align_type = weasel::UIStyle::ALIGN_CENTER;
  style.label_text_format = L"%s";
  style.min_width = 10;
  style.max_height = 600;
  style.margin_x = 8;
  style.margin_y = 8;
  style.spacing = 13;
  style.candidate_spacing = 22;
  style.hilite_spacing = 6;
  style.hilite_padding_x = 8;
  style.hilite_padding_y = 8;
  style.round_corner = 8;
  style.round_corner_ex = 8;
  style.border = 2;
  style.text_color = Color(32, 33, 36);
  style.candidate_text_color = Color(32, 33, 36);
  style.candidate_back_color = Color(255, 255, 255);
  style.candidate_border_color = Color(224, 228, 235);
  style.label_text_color = Color(92, 99, 112);
  style.comment_text_color = Color(110, 118, 130);
  style.back_color = Color(255, 255, 255);
  style.border_color = Color(224, 228, 235);
  style.hilited_text_color = Color(255, 255, 255);
  style.hilited_back_color = Color(37, 99, 235);
  style.hilited_candidate_text_color = Color(255, 255, 255);
  style.hilited_candidate_back_color = Color(37, 99, 235);
  style.hilited_candidate_border_color = Color(37, 99, 235);
  style.hilited_label_text_color = Color(255, 255, 255);
  style.hilited_comment_text_color = Color(235, 238, 245);
  style.shadow_color = Color(0, 0, 0, 0x30);
  style.shadow_radius = 8;
  style.shadow_offset_x = 2;
  style.shadow_offset_y = 3;
  style.antialias_mode = weasel::UIStyle::DEFAULT;
  style.hover_type = weasel::UIStyle::NONE;
}

bool ReadThemeFile(const std::filesystem::path& path, ThemeDocument& document) {
  std::ifstream input(path, std::ios::binary);
  if (!input) return false;

  int style_indent = -1;
  int layout_indent = -1;
  int schemes_indent = -1;
  int scheme_indent = -1;
  std::string current_scheme;
  int patch_indent = -1;
  std::vector<PatchFrame> patch_stack;
  std::string line;
  bool first_line = true;
  while (std::getline(input, line)) {
    if (first_line && line.size() >= 3 &&
        static_cast<unsigned char>(line[0]) == 0xef &&
        static_cast<unsigned char>(line[1]) == 0xbb &&
        static_cast<unsigned char>(line[2]) == 0xbf) {
      line.erase(0, 3);
    }
    first_line = false;
    const size_t indent = line.find_first_not_of(" \t");
    if (indent == std::string::npos) continue;
    const std::string content = StripComment(line.substr(indent));
    if (content.empty() || content.front() == '-') continue;
    const size_t colon = content.find(':');
    if (colon == std::string::npos) continue;
    const std::string key = Unquote(Trim(content.substr(0, colon)));
    const std::string value = Trim(content.substr(colon + 1));

    if (key == "patch" && value.empty()) {
      patch_indent = static_cast<int>(indent);
      patch_stack.clear();
      style_indent = -1;
      layout_indent = -1;
      schemes_indent = -1;
      scheme_indent = -1;
      current_scheme.clear();
      continue;
    }

    if (patch_indent >= 0) {
      if (static_cast<int>(indent) <= patch_indent) {
        patch_indent = -1;
        patch_stack.clear();
      } else {
        while (!patch_stack.empty() && patch_stack.back().indent >= static_cast<int>(indent)) {
          patch_stack.pop_back();
        }
        std::string path = key;
        if (!patch_stack.empty()) {
          path = patch_stack.back().path + "/" + path;
        }
        path = NormalizePatchPath(std::move(path));
        if (value.empty()) {
          patch_stack.push_back(PatchFrame{static_cast<int>(indent), std::move(path)});
        } else {
          ApplyPatchValue(document, std::move(path), value);
        }
        continue;
      }
    }

    if (key == "style" && value.empty()) {
      style_indent = static_cast<int>(indent);
      layout_indent = -1;
      continue;
    }
    if (key == "preset_color_schemes" && value.empty()) {
      schemes_indent = static_cast<int>(indent);
      scheme_indent = -1;
      current_scheme.clear();
      continue;
    }

    if (schemes_indent >= 0 && static_cast<int>(indent) <= schemes_indent) {
      schemes_indent = -1;
      scheme_indent = -1;
      current_scheme.clear();
    }
    if (style_indent >= 0 && static_cast<int>(indent) <= style_indent) {
      style_indent = -1;
      layout_indent = -1;
    }

    if (schemes_indent >= 0) {
      if (static_cast<int>(indent) > schemes_indent && value.empty() &&
          (current_scheme.empty() || static_cast<int>(indent) <= scheme_indent)) {
        current_scheme = key;
        scheme_indent = static_cast<int>(indent);
        document.schemes[current_scheme];
        continue;
      }
      if (!current_scheme.empty() && scheme_indent >= 0 &&
          static_cast<int>(indent) > scheme_indent) {
        document.schemes[current_scheme][key] = value;
        continue;
      }
    }

    if (style_indent >= 0 && static_cast<int>(indent) > style_indent) {
      if (key == "layout" && value.empty()) {
        layout_indent = static_cast<int>(indent);
        continue;
      }
      if (layout_indent >= 0 && static_cast<int>(indent) > layout_indent) {
        document.layout[key] = value;
      } else {
        document.style[key] = value;
      }
    }
  }
  return true;
}

std::filesystem::path ModuleDirectory() {
  wchar_t buffer[MAX_PATH]{};
  const DWORD length = GetModuleFileNameW(g_instance, buffer, ARRAYSIZE(buffer));
  if (length == 0 || length >= ARRAYSIZE(buffer)) return {};
  std::filesystem::path path(buffer, buffer + length);
  return path.parent_path();
}

std::filesystem::path EnvironmentPath(const wchar_t* name) {
  wchar_t buffer[32768]{};
  const DWORD length = GetEnvironmentVariableW(name, buffer, ARRAYSIZE(buffer));
  if (length == 0 || length >= ARRAYSIZE(buffer)) return {};
  return std::filesystem::path(buffer, buffer + length);
}

void LoadTheme(weasel::UIStyle& style) {
  SetDefaultStyle(style);
  try {
  ThemeDocument document;
  const std::filesystem::path module = ModuleDirectory();
  const std::filesystem::path appdata = EnvironmentPath(L"APPDATA");
  const std::filesystem::path explicit_path = EnvironmentPath(L"LIME_WEASEL_YAML");
  const std::filesystem::path bundled = module / L"resources" / L"rime" / L"weasel.yaml";
  const std::filesystem::path packaged = module / L"rime" / L"weasel.yaml";
  const std::filesystem::path beside_dll = module / L"weasel.yaml";
  const std::filesystem::path user_yaml = appdata / L"Rime" / L"weasel.yaml";
  const std::filesystem::path user_custom = appdata / L"Rime" / L"weasel.custom.yaml";

  if (!bundled.empty() && std::filesystem::exists(bundled)) ReadThemeFile(bundled, document);
  if (!packaged.empty() && std::filesystem::exists(packaged)) ReadThemeFile(packaged, document);
  if (!beside_dll.empty() && std::filesystem::exists(beside_dll)) ReadThemeFile(beside_dll, document);
  if (!user_yaml.empty() && std::filesystem::exists(user_yaml)) ReadThemeFile(user_yaml, document);
  if (!user_custom.empty() && std::filesystem::exists(user_custom)) ReadThemeFile(user_custom, document);
  if (!explicit_path.empty() && std::filesystem::exists(explicit_path)) ReadThemeFile(explicit_path, document);

  auto set_string = [&](std::string_view key, std::wstring& target) {
    std::string value;
    if (ParseString(document.style, key, value)) target = string_to_wstring(value, CP_UTF8);
  };
  auto set_int = [&](std::string_view key, int& target) { ParseInt(document.style, key, target); };
  auto set_bool = [&](std::string_view key, bool& target) { ParseBool(document.style, key, target); };
  set_string("font_face", style.font_face);
  set_string("label_font_face", style.label_font_face);
  set_string("comment_font_face", style.comment_font_face);
  set_string("label_format", style.label_text_format);
  set_string("mark_text", style.mark_text);
  set_int("font_point", style.font_point);
  set_int("label_font_point", style.label_font_point);
  set_int("comment_font_point", style.comment_font_point);
  set_int("candidate_abbreviate_length", style.candidate_abbreviate_length);
  set_bool("inline_preedit", style.inline_preedit);
  set_bool("display_tray_icon", style.display_tray_icon);
  set_bool("ascii_tip_follow_cursor", style.ascii_tip_follow_cursor);
  set_bool("paging_on_scroll", style.paging_on_scroll);
  set_bool("enhanced_position", style.enhanced_position);
  set_bool("click_to_capture", style.click_to_capture);
  set_bool("vertical_text_left_to_right", style.vertical_text_left_to_right);
  set_bool("vertical_text_with_wrap", style.vertical_text_with_wrap);
  set_bool("vertical_auto_reverse", style.vertical_auto_reverse);

  std::string enum_value;
  if (ParseString(document.style, "preedit_type", enum_value)) {
    if (enum_value == "preview") style.preedit_type = weasel::UIStyle::PREVIEW;
    else if (enum_value == "preview_all") style.preedit_type = weasel::UIStyle::PREVIEW_ALL;
    else style.preedit_type = weasel::UIStyle::COMPOSITION;
  }
  if (ParseString(document.style, "antialias_mode", enum_value)) {
    if (enum_value == "cleartype") style.antialias_mode = weasel::UIStyle::CLEARTYPE;
    else if (enum_value == "grayscale") style.antialias_mode = weasel::UIStyle::GRAYSCALE;
    else if (enum_value == "aliased") style.antialias_mode = weasel::UIStyle::ALIASED;
    else style.antialias_mode = weasel::UIStyle::DEFAULT;
  }
  if (ParseString(document.style, "hover_type", enum_value)) {
    if (enum_value == "hilite") style.hover_type = weasel::UIStyle::HILITE;
    else if (enum_value == "semi_hilite") style.hover_type = weasel::UIStyle::SEMI_HILITE;
    else style.hover_type = weasel::UIStyle::NONE;
  }

  bool horizontal = style.layout_type == weasel::UIStyle::LAYOUT_HORIZONTAL;
  bool fullscreen = false;
  bool vertical_text = false;
  set_bool("horizontal", horizontal);
  set_bool("fullscreen", fullscreen);
  set_bool("vertical_text", vertical_text);
  if (vertical_text) style.layout_type = weasel::UIStyle::LAYOUT_VERTICAL_TEXT;
  else if (horizontal) {
    style.layout_type = fullscreen ? weasel::UIStyle::LAYOUT_HORIZONTAL_FULLSCREEN
                                   : weasel::UIStyle::LAYOUT_HORIZONTAL;
  } else {
    style.layout_type = fullscreen ? weasel::UIStyle::LAYOUT_VERTICAL_FULLSCREEN
                                   : weasel::UIStyle::LAYOUT_VERTICAL;
  }
  if (ParseString(document.style, "layout_type", enum_value) ||
      ParseString(document.layout, "type", enum_value)) {
    if (enum_value == "horizontal") style.layout_type = weasel::UIStyle::LAYOUT_HORIZONTAL;
    else if (enum_value == "vertical_text") style.layout_type = weasel::UIStyle::LAYOUT_VERTICAL_TEXT;
    else if (enum_value == "vertical+fullscreen") style.layout_type = weasel::UIStyle::LAYOUT_VERTICAL_FULLSCREEN;
    else if (enum_value == "horizontal+fullscreen") style.layout_type = weasel::UIStyle::LAYOUT_HORIZONTAL_FULLSCREEN;
    else style.layout_type = weasel::UIStyle::LAYOUT_VERTICAL;
  }

  auto layout_int = [&](std::string_view key, int& target) {
    const auto it = document.layout.find(std::string(key));
    if (it == document.layout.end()) return false;
    const std::string value = Unquote(it->second);
    try {
      size_t used = 0;
      const int parsed = std::stoi(value, &used, 0);
      if (used != value.size()) return false;
      target = parsed;
      return true;
    } catch (...) {
      return false;
    }
  };
  layout_int("baseline", style.baseline);
  layout_int("linespacing", style.linespacing);
  layout_int("max_height", style.max_height);
  layout_int("max_width", style.max_width);
  layout_int("min_height", style.min_height);
  layout_int("min_width", style.min_width);
  if (!layout_int("border_width", style.border)) layout_int("border", style.border);
  layout_int("margin_x", style.margin_x);
  layout_int("margin_y", style.margin_y);
  layout_int("spacing", style.spacing);
  layout_int("candidate_spacing", style.candidate_spacing);
  layout_int("hilite_spacing", style.hilite_spacing);
  int padding = style.hilite_padding_x;
  layout_int("hilite_padding", padding);
  style.hilite_padding_x = padding;
  style.hilite_padding_y = padding;
  layout_int("hilite_padding_x", style.hilite_padding_x);
  layout_int("hilite_padding_y", style.hilite_padding_y);
  layout_int("corner_radius", style.round_corner_ex);
  layout_int("round_corner", style.round_corner);
  layout_int("shadow_radius", style.shadow_radius);
  layout_int("shadow_offset_x", style.shadow_offset_x);
  layout_int("shadow_offset_y", style.shadow_offset_y);
  if (ParseString(document.style, "align_type", enum_value) ||
      ParseString(document.layout, "align_type", enum_value)) {
    if (enum_value == "top") style.align_type = weasel::UIStyle::ALIGN_TOP;
    else if (enum_value == "bottom") style.align_type = weasel::UIStyle::ALIGN_BOTTOM;
    else style.align_type = weasel::UIStyle::ALIGN_CENTER;
  }

  std::string scheme_name = "purity_of_form_custom";
  ParseString(document.style, "color_scheme", scheme_name);
  const auto scheme_it = document.schemes.find(scheme_name);
  if (scheme_it == document.schemes.end()) return;
  const ThemeMap& scheme = scheme_it->second;
  std::string format = "abgr";
  ParseString(scheme, "color_format", format);
  auto color = [&](std::string_view key, int& target) {
    const auto it = scheme.find(std::string(key));
    if (it == scheme.end()) return;
    uint32_t value = 0;
    if (ParseColorValue(it->second, value)) target = ToAbgr(value, format);
  };
  auto color_alias = [&](std::string_view primary, std::string_view fallback,
                         int& target) {
    const auto primary_it = scheme.find(std::string(primary));
    const auto it = primary_it == scheme.end() ? scheme.find(std::string(fallback))
                                                : primary_it;
    if (it == scheme.end()) return;
    uint32_t value = 0;
    if (ParseColorValue(it->second, value)) target = ToAbgr(value, format);
  };
  color("back_color", style.back_color);
  color("shadow_color", style.shadow_color);
  color("prevpage_color", style.prevpage_color);
  color("nextpage_color", style.nextpage_color);
  color("text_color", style.text_color);
  color("candidate_text_color", style.candidate_text_color);
  color("candidate_back_color", style.candidate_back_color);
  color("candidate_shadow_color", style.candidate_shadow_color);
  color("candidate_border_color", style.candidate_border_color);
  color("label_color", style.label_text_color);
  color("comment_text_color", style.comment_text_color);
  color("border_color", style.border_color);
  color("hilited_text_color", style.hilited_text_color);
  color("hilited_back_color", style.hilited_back_color);
  color("hilited_shadow_color", style.hilited_shadow_color);
  color("hilited_candidate_text_color", style.hilited_candidate_text_color);
  color("hilited_candidate_back_color", style.hilited_candidate_back_color);
  color("hilited_candidate_shadow_color", style.hilited_candidate_shadow_color);
  color("hilited_candidate_border_color", style.hilited_candidate_border_color);
  color_alias("hilited_candidate_label_color", "hilited_label_color",
              style.hilited_label_text_color);
  color("hilited_comment_text_color", style.hilited_comment_text_color);
  color("hilited_mark_color", style.hilited_mark_color);
  } catch (...) {
    // A malformed or inaccessible user theme must not prevent the TSF UI
    // thread from starting.  Keep the known-good Weasel-compatible defaults.
    SetDefaultStyle(style);
  }
}

}  // namespace

WeaselUiAdapter::~WeaselUiAdapter() { Stop(); }

RECT WeaselUiAdapter::Anchor(ITfContext* context) const {
  HWND context_window = nullptr;
  Microsoft::WRL::ComPtr<ITfContextView> context_view;
  if (context && SUCCEEDED(context->GetActiveView(&context_view)) && context_view)
    context_view->GetWnd(&context_window);
  GUITHREADINFO info{};
  info.cbSize = sizeof(info);
  const HWND foreground = GetForegroundWindow();
  const HWND reference = context_window ? context_window : foreground;
  const DWORD thread_id = reference ? GetWindowThreadProcessId(reference, nullptr) : 0;
  if (thread_id && GetGUIThreadInfo(thread_id, &info) &&
      info.hwndCaret && info.rcCaret.bottom > info.rcCaret.top) {
    // GUITHREADINFO reports rcCaret in hwndCaret's client coordinates, while
    // WeaselUI expects a screen-space anchor.  Using the client rectangle
    // directly makes the popup appear near the desktop origin and only move
    // horizontally as the host caret advances.
    POINT top_left{info.rcCaret.left, info.rcCaret.top};
    POINT bottom_right{info.rcCaret.right, info.rcCaret.bottom};
    if (ClientToScreen(info.hwndCaret, &top_left) &&
        ClientToScreen(info.hwndCaret, &bottom_right)) {
      const RECT result{top_left.x, top_left.y, bottom_right.x, bottom_right.y};
      return result;
    }
  }
  HWND hwnd = context_window;
  RECT rect{200, 200, 200, 220};
  if (hwnd && GetWindowRect(hwnd, &rect)) {
    rect.left += 16;
    rect.right = rect.left + 1;
    rect.top += 32;
    rect.bottom = rect.top + 20;
  }
  return rect;
}

void WeaselUiAdapter::Show(ITfContext* context,
                           const std::vector<TextService::Candidate>& candidates,
                           size_t page,
                           size_t selected,
                           size_t page_size,
                           std::wstring_view preedit,
                           std::wstring_view preceding,
                           const RECT* anchor) {
  Snapshot snapshot;
  snapshot.visible = true;
  Microsoft::WRL::ComPtr<ITfContextView> view;
  if (context && SUCCEEDED(context->GetActiveView(&view)) && view)
    view->GetWnd(&snapshot.target_window);
  if (!snapshot.target_window) snapshot.target_window = GetForegroundWindow();
  snapshot.anchor = anchor ? *anchor : Anchor(context);
  if (!preceding.empty()) snapshot.preceding.assign(preceding.data(), preceding.size());
  // The unconfirmed pinyin is a real TSF composition rendered by the host
  // editor.  Never copy it into Weasel's Context: doing so creates a second
  // visible preedit row in themes that disable inline-preedit capability.
  (void)preedit;
  // Keep the visual page model well-defined even when a stale/invalid
  // snapshot arrives while the TSF thread is changing composition.  Weasel
  // uses zero-based currentPage and a count in totalPages; rows and the
  // highlighted index are local to the selected page.
  const size_t effective_page_size = (std::max)(size_t{1}, page_size);
  snapshot.total_pages = candidates.empty()
                             ? 0
                             : (candidates.size() - 1) / effective_page_size + 1;
  snapshot.page = snapshot.total_pages == 0
                      ? 0
                      : (std::min)(page, snapshot.total_pages - 1);
  const size_t begin = snapshot.page * effective_page_size;
  const size_t count = begin < candidates.size()
                           ? (std::min)(effective_page_size,
                                        candidates.size() - begin)
                           : 0;
  const size_t end = begin + count;
  snapshot.selected = (begin < end && selected >= begin && selected < end)
                          ? selected
                          : begin;
  snapshot.rows.reserve(count);
  for (size_t i = begin; i < end; ++i) {
    snapshot.rows.push_back(Row{candidates[i].display, i == snapshot.selected});
  }
  Update(std::move(snapshot));
}

void WeaselUiAdapter::ShowStatus(ITfContext* context, std::wstring_view message) {
  Snapshot snapshot;
  snapshot.visible = true;
  snapshot.is_status = true;
  Microsoft::WRL::ComPtr<ITfContextView> view;
  if (context && SUCCEEDED(context->GetActiveView(&view)) && view)
    view->GetWnd(&snapshot.target_window);
  if (!snapshot.target_window) snapshot.target_window = GetForegroundWindow();
  snapshot.anchor = Anchor(context);
  if (!message.empty()) snapshot.status.assign(message.data(), message.size());
  Update(std::move(snapshot));
}

void WeaselUiAdapter::Hide() {
  if (!thread_.joinable()) return;
  Snapshot snapshot;
  snapshot.visible = false;
  Update(std::move(snapshot));
}

void WeaselUiAdapter::Update(Snapshot snapshot) {
  Ensure();
  HWND hwnd = nullptr;
  {
    std::lock_guard lock(state_mutex_);
    if (stopped_) return;
    snapshot_ = std::move(snapshot);
    hwnd = host_window_;
  }
  if (hwnd) PostMessageW(hwnd, kUpdateMessage, 0, 0);
}

void WeaselUiAdapter::Ensure() {
  std::call_once(start_once_, [this] {
    thread_ = std::thread([this] { UiThread(); });
    std::unique_lock lock(ready_mutex_);
    ready_cv_.wait(lock, [this] { return ready_; });
  });
}

void WeaselUiAdapter::Stop() {
  if (!thread_.joinable()) return;
  {
    std::unique_lock lock(ready_mutex_);
    ready_cv_.wait(lock, [this] { return ready_; });
  }
  HWND hwnd = nullptr;
  DWORD thread_id = 0;
  {
    std::lock_guard lock(state_mutex_);
    stopped_ = true;
    hwnd = host_window_;
    thread_id = ui_thread_id_;
  }
  if (hwnd) PostMessageW(hwnd, WM_CLOSE, 0, 0);
  else if (thread_id) PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
  thread_.join();
}

LRESULT CALLBACK WeaselUiAdapter::HostProc(HWND hwnd, UINT message, WPARAM wparam,
                                           LPARAM lparam) {
  auto* self = reinterpret_cast<WeaselUiAdapter*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
  if (message == WM_NCCREATE) {
    self = static_cast<WeaselUiAdapter*>(reinterpret_cast<CREATESTRUCTW*>(lparam)->lpCreateParams);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(self));
  }
  if (message == kUpdateMessage && self) {
    self->Render();
    return 0;
  }
  if (message == WM_CLOSE) {
    DestroyWindow(hwnd);
    return 0;
  }
  if (message == WM_DESTROY) {
    PostQuitMessage(0);
    return 0;
  }
  return DefWindowProcW(hwnd, message, wparam, lparam);
}

void WeaselUiAdapter::UiThread() {
  ui_thread_id_ = GetCurrentThreadId();
  const HRESULT coinit = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  const bool com_usable = SUCCEEDED(coinit) || coinit == RPC_E_CHANGED_MODE;

  WNDCLASSW wc{};
  wc.lpfnWndProc = &WeaselUiAdapter::HostProc;
  wc.hInstance = g_instance;
  wc.lpszClassName = L"LimeWeaselUiHostV1";
  RegisterClassW(&wc);
  HWND host = CreateWindowExW(WS_EX_TOOLWINDOW, wc.lpszClassName, L"Lime WeaselUI host",
                              WS_POPUP, 0, 0, 1, 1, nullptr, nullptr, g_instance, this);
  {
    std::lock_guard lock(state_mutex_);
    host_window_ = host;
  }

  {
    std::lock_guard lock(ready_mutex_);
    ready_ = true;
  }
  ready_cv_.notify_all();

  // Signal readiness even when the host window or COM apartment cannot be
  // created.  Stop() can then join a thread that has already exited instead
  // of waiting forever for a message that will never arrive.
  if (!host || !com_usable) {
    if (host && IsWindow(host)) DestroyWindow(host);
    {
      std::lock_guard lock(state_mutex_);
      host_window_ = nullptr;
    }
    if (SUCCEEDED(coinit)) CoUninitialize();
    return;
  }

  ConfigureStyle();
  ui_.InServer() = true;
  ui_.SetUICallBack([this](size_t* const selected, size_t* const hovered,
                           bool* const next_page,
                           bool* const scroll_next_page) {
    (void)hovered;
    bool visible = false;
    bool is_status = false;
    HWND target_window = nullptr;
    {
      std::lock_guard lock(state_mutex_);
      visible = snapshot_.visible;
      is_status = snapshot_.is_status;
      target_window = snapshot_.target_window;
    }
    if (!visible || is_status) return;
    const HWND foreground = GetForegroundWindow();
    const HWND target_root = target_window ? GetAncestor(target_window, GA_ROOT) : nullptr;
    const HWND foreground_root = foreground ? GetAncestor(foreground, GA_ROOT) : nullptr;
    if (!target_root || !foreground_root || target_root != foreground_root) return;
    if (selected && *selected < 9) {
      // Candidate labels are the same 1..9 shortcuts handled by TextService.
      // SendInput keeps the TSF context and commit on the TSF thread.
      InjectVirtualKey(static_cast<WORD>('1' + *selected));
    } else if (next_page) {
      InjectVirtualKey(*next_page ? VK_NEXT : VK_PRIOR);
    } else if (scroll_next_page) {
      const bool page = ui_.style().paging_on_scroll;
      InjectVirtualKey(page ? (*scroll_next_page ? VK_NEXT : VK_PRIOR)
                            : (*scroll_next_page ? VK_DOWN : VK_UP));
    }
  });
  MSG message{};
  while (GetMessageW(&message, nullptr, 0, 0) > 0) {
    TranslateMessage(&message);
    DispatchMessageW(&message);
  }

  if (ui_created_) {
    ui_.Hide();
    ui_.Destroy(true);
    ui_created_ = false;
    ui_parent_ = nullptr;
  }
  if (host && IsWindow(host)) DestroyWindow(host);
  {
    std::lock_guard lock(state_mutex_);
    host_window_ = nullptr;
  }
  if (SUCCEEDED(coinit)) CoUninitialize();
}

bool WeaselUiAdapter::EnsureUiCreated(HWND parent) {
  if (parent && !IsWindow(parent)) parent = nullptr;
  if (ui_created_ && (!IsWindow(ui_parent_) || ui_parent_ != parent)) {
    ui_.Hide();
    // Keep Weasel's DirectWrite resources and style, but recreate the popup
    // with the active TSF view as owner.  A cached owner belongs to the prior
    // focused control and causes stale placement or UWP clipping.
    ui_.Destroy(false);
    ui_created_ = false;
    ui_parent_ = nullptr;
  }
  if (ui_created_) return true;
  ui_created_ = ui_.Create(parent);
  if (ui_created_) ui_parent_ = parent;
  if (!ui_created_ && parent) {
    ui_created_ = ui_.Create(nullptr);
    if (ui_created_) ui_parent_ = nullptr;
  }
  return ui_created_;
}

void WeaselUiAdapter::Render() {
  Snapshot snapshot;
  {
    std::lock_guard lock(state_mutex_);
    snapshot = snapshot_;
  }
  if (!snapshot.visible) {
    if (ui_created_) ui_.Hide();
    return;
  }

  if (!EnsureUiCreated(snapshot.target_window)) return;

  weasel::Context context;
  weasel::Status status;
  if (snapshot.is_status) {
    context.aux.str = snapshot.status;
    status.schema_name = L"Lime";
    status.schema_id = L"lime";
    status.composing = false;
    ui_.SetPrecedingText({});
    ui_.Update(context, status);
    ui_.UpdateInputPosition(snapshot.anchor);
    ui_.ShowWithTimeout(1800);
    return;
  }

  // Keep the popup focused on the auxiliary preceding-text row and candidates;
  // the host textbox owns and renders the unconfirmed composition.
  context.preedit.str.clear();
  context.cinfo.currentPage = static_cast<int>(snapshot.page);
  context.cinfo.totalPages = static_cast<int>(snapshot.total_pages);
  context.cinfo.highlighted = 0;
  context.cinfo.is_last_page = snapshot.total_pages == 0 ||
                               snapshot.page + 1 >= snapshot.total_pages;
  for (size_t i = 0; i < snapshot.rows.size(); ++i) {
    context.cinfo.candies.emplace_back(snapshot.rows[i].display);
    context.cinfo.comments.emplace_back(L"");
    context.cinfo.labels.emplace_back(std::to_wstring(i + 1));
    if (snapshot.rows[i].selected) context.cinfo.highlighted = static_cast<int>(i);
  }
  status.schema_name = L"Lime";
  status.schema_id = L"lime";
  status.composing = true;
  ui_.SetPrecedingText(snapshot.preceding);
  ui_.Update(context, status);
  ui_.UpdateInputPosition(snapshot.anchor);
  ui_.Show();
}

void WeaselUiAdapter::ConfigureStyle() {
  LoadTheme(ui_.style());
  // The TSF host owns the composition text and Render() leaves Context::preedit
  // empty.  Keep Weasel's inline-preedit mode so theme overrides cannot
  // reintroduce a duplicate pinyin row in the popup.
  ui_.style().inline_preedit = true;
  ui_.style().client_caps |= weasel::INLINE_PREEDIT_CAPABLE;
}

}  // namespace lime::tsf
