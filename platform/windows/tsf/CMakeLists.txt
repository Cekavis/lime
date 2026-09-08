cmake_minimum_required(VERSION 3.20)

project(lime_tsf LANGUAGES CXX)

if(NOT WIN32)
  message(FATAL_ERROR "lime_tsf is a Windows-only TSF adapter")
endif()

option(LIME_WITH_WEASEL_UI "Render the native candidate window with WeaselUI" ON)
option(LIME_WEASEL_UI_OPENMP "Enable OpenMP acceleration for WeaselUI blur rendering" OFF)

set(LIME_WEASEL_UI_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../third_party/weasel-ui")
set(LIME_TSF_SOURCES
    tsf_module.cpp
    tsf_module.h
    lime_tsf.cpp
    lime_tsf.h
    weasel_ui_adapter.cpp
    weasel_ui_adapter.h)

add_library(lime_tsf SHARED ${LIME_TSF_SOURCES})
target_compile_features(lime_tsf PRIVATE cxx_std_20)
target_compile_definitions(lime_tsf PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_compile_definitions(lime_tsf PRIVATE LIME_TSF_EXPORTS)
if (MSVC)
  target_compile_options(lime_tsf PRIVATE /utf-8)
endif()
set_target_properties(lime_tsf PROPERTIES OUTPUT_NAME "lime-tsf")

target_link_libraries(lime_tsf PRIVATE ole32 advapi32 user32 shlwapi)

if (LIME_WITH_WEASEL_UI AND EXISTS "${LIME_WEASEL_UI_DIR}/src/WeaselUI.cpp")
  set(LIME_WEASEL_UI_SOURCES
      "${LIME_WEASEL_UI_DIR}/src/DirectWriteResources.cpp"
      "${LIME_WEASEL_UI_DIR}/src/FullScreenLayout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/GdiplusBlur.cpp"
      "${LIME_WEASEL_UI_DIR}/src/HorizontalLayout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/Layout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/StandardLayout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/VerticalLayout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/VHorizontalLayout.cpp"
      "${LIME_WEASEL_UI_DIR}/src/WeaselPanel.cpp"
      "${LIME_WEASEL_UI_DIR}/src/WeaselUI.cpp"
      "${LIME_WEASEL_UI_DIR}/src/stdafx.cpp")
  add_library(lime_weasel_ui STATIC ${LIME_WEASEL_UI_SOURCES})
  target_compile_features(lime_weasel_ui PRIVATE cxx_std_20)
  target_compile_definitions(lime_weasel_ui PRIVATE
      UNICODE _UNICODE WIN32_LEAN_AND_MEAN GDIPVER=0x0110)
  target_include_directories(lime_weasel_ui PUBLIC
      "${LIME_WEASEL_UI_DIR}/include"
      "${LIME_WEASEL_UI_DIR}/src")
  if (MSVC)
    target_compile_options(lime_weasel_ui PRIVATE /utf-8)
    if (LIME_WEASEL_UI_OPENMP)
      target_compile_options(lime_weasel_ui PRIVATE /openmp)
    endif()
  endif()
  target_link_libraries(lime_weasel_ui PUBLIC
      d2d1 dwrite gdiplus shcore user32 gdi32 shell32 ole32 uuid
      comctl32 uxtheme msimg32 dwmapi propsys)

  target_compile_definitions(lime_tsf PRIVATE LIME_WITH_WEASEL_UI)
  target_include_directories(lime_tsf PRIVATE
      "${LIME_WEASEL_UI_DIR}/include"
      "${LIME_WEASEL_UI_DIR}/src")
  target_link_libraries(lime_tsf PRIVATE lime_weasel_ui)
elseif (LIME_WITH_WEASEL_UI)
  message(WARNING "LIME_WITH_WEASEL_UI requested but third_party/weasel-ui is missing; using the built-in renderer")
endif()
