import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.parcelize")
    id("rust")
}

val signingProperties = Properties().apply {
    val source = System.getenv("DAAT_LOCUS_ANDROID_SIGNING")
    if (source != null) file(source).inputStream().use { load(it) }
}

repositories {
    maven {
        url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
        content { includeModule("org.rustls", "rustls-platform-verifier") }
    }
}

// Cargo pins the matching JVM verifier implementation, including its Maven version.
val verifierVersion = providers.exec {
    workingDir(file("../../../../../../"))
    commandLine("cargo", "metadata", "--manifest-path", file("../../../../../../Cargo.toml").absolutePath, "--format-version", "1", "--locked", "--filter-platform", "aarch64-linux-android")
}.standardOutput.asText.map { output ->
    val metadata = groovy.json.JsonSlurper().parseText(output) as Map<*, *>
    @Suppress("UNCHECKED_CAST")
    val packages = metadata["packages"] as List<Map<*, *>>
    val verifier = packages.single { it["name"] == "rustls-platform-verifier-android" }
    verifier["version"] as String
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 37
    ndkVersion = "27.2.12479018"
    namespace = "io.daat_locus.app"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "io.daat_locus.app"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        testInstrumentationRunner = "io.daat_locus.app.ReleaseStartupTest"
    }
    // Startup tests must exercise the same R8/JNI boundary as the distributed APK.
    testBuildType = providers.gradleProperty("daatTestBuildType").getOrElse("release")
    signingConfigs {
        create("release") {
            if (!signingProperties.isEmpty) {
                keyAlias = signingProperties.getProperty("keyAlias")
                keyPassword = signingProperties.getProperty("password")
                storeFile = file(signingProperties.getProperty("storeFile"))
                storePassword = signingProperties.getProperty("password")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
    packaging {
        // Compress the distributable APK; Android extracts the library at install time.
        jniLibs.useLegacyPackaging = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation(verifierVersion.map { "org.rustls:rustls-platform-verifier:$it" })
    implementation("androidx.webkit:webkit:1.17.1")
    implementation("androidx.appcompat:appcompat:1.8.0")
    implementation("androidx.activity:activity-ktx:1.13.0")
    implementation("com.google.android.material:material:1.14.0")
    implementation("androidx.lifecycle:lifecycle-process:2.11.0")
    implementation("dev.rikka.shizuku:api:13.1.5")
    implementation("dev.rikka.shizuku:provider:13.1.5")
    implementation("be.mygod.librootkotlinx:librootkotlinx:2.0.0-beta02")
    testImplementation("junit:junit:4.13.2")
}

apply(from = file("tauri.build.gradle.kts"))
