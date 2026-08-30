cmake_minimum_required(VERSION 3.20)

project(lime_tsf LANGUAGES CXX)

if(NOT WIN32)
  message(FATAL_ERROR "lime_tsf is a Windows-only TSF adapter scaffold")
endif()

add_library(lime_tsf SHARED tsf_module.cpp)
target_compile_features(lime_tsf PRIVATE cxx_std_20)
target_compile_definitions(lime_tsf PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_compile_definitions(lime_tsf PRIVATE LIME_TSF_EXPORTS)
set_target_properties(lime_tsf PROPERTIES OUTPUT_NAME "lime-tsf")

target_link_libraries(lime_tsf PRIVATE ole32)
