"""Macro building the -javaagent jar against a given server jar."""

load("@rules_java//java:defs.bzl", "java_binary", "java_library")

def hook_agent(name, server):
    java_library(
        name = name + "_lib",
        srcs = native.glob(["src/bazelhook/*.java"]),
        # The Linux x86_64 renderer, loaded by NativeUi when the server runs
        # on that platform; elsewhere the resource is absent and the UI is off.
        resources = ["//agent:native_lib"],
        resource_strip_prefix = "agent/resources",
        deps = [
            server,
            "@maven//:net_bytebuddy_byte_buddy",
        ],
    )

    # <name>_deploy.jar is the -javaagent jar: our classes plus Byte Buddy.
    java_binary(
        name = name,
        create_executable = False,
        deploy_manifest_lines = [
            "Premain-Class: bazelhook.Agent",
            "Can-Retransform-Classes: true",
        ],
        runtime_deps = [":" + name + "_lib"],
        visibility = ["//visibility:public"],
    )
