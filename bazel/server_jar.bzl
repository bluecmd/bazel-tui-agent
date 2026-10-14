"""Repository rule that fetches the Bazel release binary for the version in
.bazelversion and extracts A-server.jar from it. The agent compiles against
this jar; it must match the Bazel version that will load the agent."""

# sha256 of bazel-<v>-linux-x86_64 for versions we have verified. Other
# versions still work, Bazel just warns about the missing checksum.
_KNOWN_SHA256 = {
    "9.0.0": "c44a93f25398c68f904fa1d19b61d321de6c0d2f09dca375d7bc0dc9b9428403",
    "9.0.2": "422e7a1690b76d7e615c29091d3aca28d0bd3a93fe3c93cbefb8f72d774926d5",
    "9.1.1": "857bed5d2756b4d998d3caebf2d941d13d434c4eda4b1d6d7dda205736c25a93",
    "9.2.0": "7668a95db1250f12c40407251e4e203b4ec8bf39bc495d2f485b2d8c99048694",
    "9.3.0": "d302d22ed77ee5658b87857249baa579db84300ecebe0a1e9a357b5b20f80eb8",
    "10.0.0rc1": "cb2ce9ecf6eb11fbe56af711cd7875092b456ee53fe78fef4c6f0c6fd7d0091c",
    "8.8.1": "5b5bab2095065817d3416ca9a0678bd39c10c9ede432dbe8187bfe74de285160",
    "8.4.1": "3453736dc067a9b923f3066aad378282cd864830d9ad135a45ac9c1ebc47bbae",
    "8.2.1": "7ff2b6a675b59a791d007c526977d5262ade8fa52efc8e0d1ff9e18859909fc0",
}

_BUILD = """
load("@rules_java//java:defs.bzl", "java_import")

java_import(
    name = "server",
    jars = ["A-server.jar"],
    # Never bundle the server jar into the agent: at runtime the agent shares
    # the system classpath with A-server.jar itself.
    neverlink = True,
    visibility = ["//visibility:public"],
)

filegroup(
    name = "jar",
    srcs = ["A-server.jar"],
    visibility = ["//visibility:public"],
)
"""

def _impl(rctx):
    version = rctx.attr.version or rctx.read(rctx.attr.version_file).strip()
    url = "https://github.com/bazelbuild/bazel/releases/download/{v}/bazel-{v}-linux-x86_64".format(v = version)
    rctx.report_progress("Downloading Bazel %s release binary" % version)
    rctx.download(
        url = url,
        output = "bazel-release",
        sha256 = _KNOWN_SHA256.get(version, ""),
    )

    # The release binary is an ELF launcher with a zip archive appended.
    # Python's zipfile copes with the prefix; `unzip` only warns about it.
    rctx.report_progress("Extracting A-server.jar")
    res = rctx.execute([
        "python3",
        "-I",
        "-c",
        "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extract('A-server.jar', '.')",
        "bazel-release",
    ])
    if res.return_code != 0:
        fail("Could not extract A-server.jar from %s: %s" % (url, res.stderr))
    rctx.delete("bazel-release")
    rctx.file("version.txt", version + "\n")
    rctx.file("BUILD.bazel", _BUILD)

bazel_server_jar = repository_rule(
    implementation = _impl,
    attrs = {
        "version": attr.string(doc = "Bazel version. Overrides version_file."),
        "version_file": attr.label(allow_single_file = True, doc = "File holding the Bazel version, usually .bazelversion."),
    },
)
