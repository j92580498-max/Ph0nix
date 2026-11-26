plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val repositoryRoot = rootProject.projectDir.resolve("../..").canonicalFile
val nativeProject = rootProject.projectDir.resolve("native")
val generatedJniLibs = layout.buildDirectory.dir("generated/e830JniLibs")
val nativeLibrary = layout.buildDirectory.file("generated/e830JniLibs/arm64-v8a/libe830_android_jni.so")
val engineLibrary = layout.buildDirectory.file("generated/e830JniLibs/arm64-v8a/libdb2010_engine.so")
val engineSupport = rootProject.projectDir.resolve("build-support")
val python = System.getenv("FEATUREPHONE_PYTHON") ?: "python3"
val ndkRoot = System.getenv("ANDROID_NDK_HOME")
    ?: System.getenv("ANDROID_NDK_ROOT")
    ?: System.getenv("ANDROID_HOME")?.let { "$it/ndk/29.0.14206865" }
    ?: System.getenv("ANDROID_SDK_ROOT")?.let { "$it/ndk/29.0.14206865" }
    ?: repositoryRoot.resolve("build/android/ndk/android-ndk-r29").absolutePath

android {
    namespace = "com.samsung.e830"
    compileSdk = 35
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "org.featurephone.emulator"
        minSdk = 24
        targetSdk = 35
        versionCode = 9
        versionName = "1.0"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    sourceSets["main"].jniLibs.srcDir(generatedJniLibs)
    buildTypes {
        getByName("release") {
            // A development certificate makes standalone builds installable.
            // Published builds retain the project's existing upgrade certificate.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    packaging {
        jniLibs {
            // Android permits executing an extracted, packaged native PIE.
            useLegacyPackaging = true
            keepDebugSymbols += listOf("**/*.so")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
    }

}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

val buildE830Native by tasks.registering(Exec::class) {
    group = "build"
    description = "Cross-compiles the phone firmware worker for Android ARM64."
    workingDir = repositoryRoot
    commandLine(python, rootProject.projectDir.resolve("build-support/build_native.py"), "--ndk", ndkRoot, "--jobs", "2")
    inputs.files(
        repositoryRoot.resolve("Cargo.toml"),
        repositoryRoot.resolve("Cargo.lock"),
        repositoryRoot.resolve(".cargo/config.toml"),
        nativeProject.resolve("Cargo.toml"),
        nativeProject.resolve("build.rs"),
    )
    inputs.dir(repositoryRoot.resolve("native"))
    inputs.dir(repositoryRoot.resolve("dependencies/samsung-runtime/src"))
    inputs.dir(nativeProject.resolve("src"))
    inputs.files(engineSupport.resolve("build_native.py"), engineSupport.resolve("build_qemu.py"))
    inputs.file(rootProject.projectDir.resolve("build-support/cmake/android-arm64-v8a.toolchain.cmake"))
    outputs.file(nativeLibrary)
}

val buildDb2010Engine by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds the native Android DB2010 engine and static dependencies."
    workingDir = repositoryRoot
    commandLine(python, rootProject.projectDir.resolve("build-support/build_qemu.py"), "--ndk", ndkRoot, "--jobs", "2")
    inputs.files(repositoryRoot.resolve("dependencies/qemu/source-lock.json"), engineSupport.resolve("build_qemu.py"),
        engineSupport.resolve("android_compat.c"), engineSupport.resolve("android_compat.h"),
        repositoryRoot.resolve("src/db2010/development/source.py"), repositoryRoot.resolve("src/db2010/paths.py"))
    inputs.dir(repositoryRoot.resolve("dependencies/qemu/patches"))
    inputs.files(fileTree(repositoryRoot.resolve("qemu")) {
        include("**/*.c", "**/*.h", "**/*.S", "**/*.inc", "**/*.hx", "**/*.json", "**/*.py", "**/*.build",
            "**/*.mak", "**/*.wrap", "**/Kconfig*", "**/meson*", "**/trace-events", "configure", "VERSION")
        exclude("build/**", ".git/**", "**/__pycache__/**")
    })
    outputs.file(engineLibrary)
}

tasks.named("preBuild") {
    dependsOn(buildDb2010Engine, buildE830Native)
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.1")
    implementation("androidx.coordinatorlayout:coordinatorlayout:1.2.0")
    implementation("androidx.recyclerview:recyclerview:1.3.2")
    implementation("com.google.android.material:material:1.12.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
}
