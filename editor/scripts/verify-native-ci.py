#!/usr/bin/env python3
"""Use only the runner's existing tools; retain unavailable native prerequisites.

On Linux, an existing Xvfb and dbus-run-session can provide a virtual X11
display and session bus. Solder still creates an actual X11 OS window and
receives XTEST input; GPUI's headless/test backends are never used.
An offline build prevents implicit provisioning on a fresh runner. A parent
workflow may instead supply --binary after its separately approved build.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


# GPUI 0.2.2's BladeContext uses blade-graphics 0.7.1 with presentation=true.
# This probe reports its requirements from vulkan/init.rs; it is diagnostic
# only, never evidence that an X11 window rendered or accepted native input.
VULKAN_PROBE = r'''
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>

static void check(VkResult result, const char *operation) {
    printf("%s: %d\n", operation, result);
    if (result != VK_SUCCESS) exit(1);
}

int main(void) {
    setbuf(stdout, NULL);
    puts("Vulkan inventory only; not a native UI check");
    uint32_t version = 0, count = 0;
    check(vkEnumerateInstanceVersion(&version), "vkEnumerateInstanceVersion");
    printf("Loader API: %u.%u.%u\n", VK_VERSION_MAJOR(version),
           VK_VERSION_MINOR(version), VK_VERSION_PATCH(version));
    check(vkEnumerateInstanceExtensionProperties(NULL, &count, NULL), "instance extension count");
    VkExtensionProperties *extensions = calloc(count ? count : 1, sizeof(*extensions));
    if (!extensions) return 1;
    check(vkEnumerateInstanceExtensionProperties(NULL, &count, extensions), "instance extensions");
    for (uint32_t i = 0; i < count; ++i) puts(extensions[i].extensionName);
    free(extensions);
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .apiVersion = VK_MAKE_VERSION(1, 3, 281)};
    VkInstanceCreateInfo create = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                  .pApplicationInfo = &app};
    VkInstance instance;
    check(vkCreateInstance(&create, NULL, &instance), "vkCreateInstance");
    check(vkEnumeratePhysicalDevices(instance, &count, NULL), "physical device count");
    printf("Physical devices: %u\n", count);
    VkPhysicalDevice *devices = calloc(count ? count : 1, sizeof(*devices));
    if (!devices) return 1;
    check(vkEnumeratePhysicalDevices(instance, &count, devices), "physical devices");
    for (uint32_t i = 0; i < count; ++i) {
        VkPhysicalDeviceDriverProperties driver = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
        VkPhysicalDeviceInlineUniformBlockProperties block = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_INLINE_UNIFORM_BLOCK_PROPERTIES,
            .pNext = &driver};
        VkPhysicalDeviceProperties2 properties = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &block};
        vkGetPhysicalDeviceProperties2(devices[i], &properties);
        printf("Device: %s type=%d vendor=%04x device=%04x API=%u.%u.%u\n",
               properties.properties.deviceName, properties.properties.deviceType,
               properties.properties.vendorID, properties.properties.deviceID,
               VK_VERSION_MAJOR(properties.properties.apiVersion),
               VK_VERSION_MINOR(properties.properties.apiVersion),
               VK_VERSION_PATCH(properties.properties.apiVersion));
        printf("Driver: %s %s\n", driver.driverName, driver.driverInfo);
        VkPhysicalDeviceDynamicRenderingFeatures rendering = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES};
        VkPhysicalDeviceTimelineSemaphoreFeatures timeline = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES,
            .pNext = &rendering};
        VkPhysicalDeviceInlineUniformBlockFeatures uniform = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_INLINE_UNIFORM_BLOCK_FEATURES,
            .pNext = &timeline};
        VkPhysicalDeviceFeatures2 features = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, .pNext = &uniform};
        vkGetPhysicalDeviceFeatures2(devices[i], &features);
        printf("Blade requires inlineUniformBlock=1, size>=256, descriptor sets>0, "
               "timelineSemaphore=1, dynamicRendering=1; got %u %u %u %u %u\n",
               uniform.inlineUniformBlock, block.maxInlineUniformBlockSize,
               block.maxDescriptorSetInlineUniformBlocks,
               timeline.timelineSemaphore, rendering.dynamicRendering);
        uint32_t extension_count = 0;
        check(vkEnumerateDeviceExtensionProperties(devices[i], NULL, &extension_count, NULL),
              "device extension count");
        extensions = calloc(extension_count ? extension_count : 1, sizeof(*extensions));
        if (!extensions) return 1;
        check(vkEnumerateDeviceExtensionProperties(devices[i], NULL, &extension_count, extensions),
              "device extensions");
        puts("Blade requires VK_EXT_inline_uniform_block, VK_KHR_timeline_semaphore, "
             "VK_KHR_descriptor_update_template, VK_KHR_dynamic_rendering, VK_KHR_swapchain:");
        for (uint32_t j = 0; j < extension_count; ++j) puts(extensions[j].extensionName);
        free(extensions);
    }
    free(devices);
    vkDestroyInstance(instance, NULL);
    return 0;
}
'''


def software_vulkan_manifests():
    # These are existing image locations, not download or installation targets.
    return [Path(directory) / name
            for directory in ("/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d")
            for name in ("lvp_icd.x86_64.json", "lvp_icd.json")] + [
        Path("/opt/google/chrome/vk_swiftshader_icd.json"),
        Path("/usr/local/share/chromium/chrome-linux/vk_swiftshader_icd.json")]


def configure_software_vulkan(env, paths):
    candidates = []
    selected = None
    for path in paths:
        candidate = {"path": str(path), "exists": path.is_file()}
        candidates.append(candidate)
        if not candidate["exists"]:
            continue
        try:
            candidate["manifest"] = json.loads(path.read_text(encoding="utf-8"))
            library = candidate["manifest"]["ICD"]["library_path"]
            if not isinstance(library, str) or not library:
                raise ValueError("ICD library_path must be a nonempty string")
            # A bare soname uses the system library search path; paths in a
            # manifest are relative to that manifest, not the working directory.
            if "/" in library:
                resolved = (path.parent / library).resolve()
                candidate["library"] = str(resolved)
                if not resolved.is_file():
                    raise ValueError("The manifest's driver library is missing")
            if selected is None:
                selected = str(path.resolve())
        except (OSError, ValueError, KeyError, TypeError) as error:
            candidate["error"] = str(error)
    if selected:
        # The newer variable wins; set both for older Vulkan loaders too.
        env["VK_DRIVER_FILES"] = env["VK_ICD_FILENAMES"] = selected
        env.pop("ZED_DEVICE_ID", None)
    return {"candidates": candidates, "selected": selected}


def diagnostic_command(out, name, command, env, timeout=20):
    with (out / name).open("w", encoding="utf-8") as log:
        log.write(json.dumps(command) + "\n")
        log.flush()
        try:
            result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                    timeout=timeout)
            log.write(f"\nExit code: {result.returncode}\n")
            return result.returncode
        except (OSError, subprocess.TimeoutExpired) as error:
            log.write(f"\nDiagnostic unavailable: {error}\n")
            return None


def gpu_diagnostics(out, env, software):
    original_env = env.copy()
    keys = ("DISPLAY", "VK_DRIVER_FILES", "VK_ICD_FILENAMES", "VK_LOADER_DEBUG", "ZED_DEVICE_ID",
            "VK_LOADER_DRIVERS_SELECT", "VK_LOADER_DRIVERS_DISABLE", "MESA_VK_DEVICE_SELECT")
    report = {"software_requested": software,
              "incoming_environment": {key: env.get(key) for key in keys}}
    if software:
        report.update(configure_software_vulkan(env, software_vulkan_manifests()))
        if not report["selected"]:
            env["SOLDER_NATIVE_GPU_UNAVAILABLE"] = (
                "No preinstalled Lavapipe or browser SwiftShader driver found; "
                "inspect gpu-diagnostics.json. No driver was downloaded or installed")
        else:
            env.pop("SOLDER_NATIVE_GPU_UNAVAILABLE", None)
    env.setdefault("VK_LOADER_DEBUG", "error,warn,driver")
    env.setdefault("RUST_BACKTRACE", "1")
    report["effective_environment"] = {key: env.get(key) for key in keys}
    (out / "gpu-diagnostics.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    diagnostic_command(out, "gpu-packages.log", ["dpkg-query", "-W", "libvulkan1",
                       "mesa-vulkan-drivers", "google-chrome-stable"], env)
    # The workflow already has cc, libvulkan-dev and the loader. Do not install
    # vulkan-tools just to diagnose a device the loader could already enumerate.
    source, binary = out / "vulkan-probe.c", out / "vulkan-probe"
    source.write_text(VULKAN_PROBE, encoding="utf-8")
    report["probe_build_exit_code"] = diagnostic_command(
        out, "vulkan-probe-build.log", ["cc", str(source), "-o", str(binary), "-lvulkan"], env, timeout=30)
    if report["probe_build_exit_code"] == 0:
        if software and report["selected"]:
            # Compare default discovery with the selected driver on the same
            # X server. Neither probe result can pass a native-window check.
            original_env.setdefault("VK_LOADER_DEBUG", "error,warn,driver")
            report["default_probe_exit_code"] = diagnostic_command(
                out, "vulkan-probe-default.log", [str(binary)], original_env)
        report["probe_exit_code"] = diagnostic_command(out, "vulkan-probe.log", [str(binary)], env)
    (out / "gpu-diagnostics.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--software-vulkan", action="store_true",
                        help="Select only an existing Linux software Vulkan driver; never install one")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    scripts = Path(__file__).resolve().parent
    with (out / "harness-tests.log").open("w", encoding="utf-8") as log:
        tests = subprocess.run([sys.executable, str(scripts / "verify-native-harness-test.py")],
                               stdout=log, stderr=subprocess.STDOUT, timeout=30)
    command = [sys.executable, str(scripts / "verify-native-notebook.py"), "--out", str(out)]
    command += ["--binary", str(args.binary.resolve())] if args.binary else ["--build"]
    env = os.environ.copy()
    env["SOLDER_NATIVE_HARNESS_TEST_EXIT_CODE"] = str(tests.returncode)
    server, server_log, display_file = None, None, None
    try:
        if sys.platform.startswith("linux"):
            # Explicitly choose this harness's X11 path. Never invoke HeadlessClient.
            env.pop("WAYLAND_DISPLAY", None)
            env.pop("ZED_HEADLESS", None)
            if not env.get("DISPLAY") and shutil.which("Xvfb"):
                display_path = out / "xvfb-display.txt"
                display_file = display_path.open("wb")
                server_log = (out / "xvfb.log").open("wb")
                server = subprocess.Popen([
                    "Xvfb", "-displayfd", str(display_file.fileno()), "-screen", "0", "1440x1000x24",
                    "-nolisten", "tcp"], pass_fds=(display_file.fileno(),),
                    stdout=server_log, stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline and server.poll() is None:
                    number = display_path.read_text(encoding="ascii").strip()
                    if number.isdecimal():
                        env["DISPLAY"] = ":" + number
                        env["SOLDER_NATIVE_DISPLAY_KIND"] = "xvfb-virtual-display"
                        break
                    time.sleep(0.1)
            if not env.get("DBUS_SESSION_BUS_ADDRESS") and shutil.which("dbus-run-session"):
                command = ["dbus-run-session", "--", *command]
            gpu_diagnostics(out, env, args.software_vulkan)
        result = subprocess.run(command, env=env, timeout=4500)
        if result.returncode:
            # Failed-run diagnostics must be readable without fetching an archive.
            # These files contain only the first-party fixture and GPU inventory.
            for name in ("gpu-diagnostics.json", "vulkan-probe-default.log", "vulkan-probe.log",
                         "process-1.log", "harness-error.log"):
                path = out / name
                if path.is_file():
                    print(f"\n{name}:\n{path.read_text(encoding='utf-8', errors='replace')[-49152:]}", flush=True)
        return result.returncode if tests.returncode == 0 else 1
    finally:
        if server and server.poll() is None:
            server.terminate()
            server.wait(timeout=10)
        for stream in (server_log, display_file):
            if stream:
                stream.close()


if __name__ == "__main__":
    sys.exit(main())
