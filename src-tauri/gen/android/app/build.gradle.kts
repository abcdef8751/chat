import groovy.json.JsonSlurper
import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

// Locate the `rustls-platform-verifier-android` AAR that cargo downloaded for
// the Android target and register it as a local Maven repo. reqwest 0.13
// routes TLS through rustls-platform-verifier, which needs its Kotlin
// component (`org.rustls.platformverifier.CertificateVerifier`) packaged into
// the APK or the trust store cannot be reached from Java.
fun rustlsPlatformVerifierMaven(): String {
    val cargoToml = project.rootDir.parentFile.parentFile.resolve("Cargo.toml")
    val metadata = providers.exec {
        commandLine(
            "cargo", "metadata", "--format-version", "1",
            "--filter-platform", "aarch64-linux-android",
            "--manifest-path", cargoToml.absolutePath,
        )
    }.standardOutput.asText.get()
    val json = JsonSlurper().parseText(metadata) as Map<*, *>
    val pkg = (json["packages"] as List<*>).first { p ->
        (p as Map<*, *>)["name"] == "rustls-platform-verifier-android"
    } as Map<*, *>
    return File(File(pkg["manifest_path"] as String).parentFile, "maven").path
}

repositories {
    google()
    mavenCentral()
    maven {
        url = uri(rustlsPlatformVerifierMaven())
        metadataSources.artifact()
    }
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release signing from a gitignored keystore.properties
val keystoreProperties = Properties()
val keystorePropsFile = project.file("keystore.properties")
if (keystorePropsFile.exists()) {
    keystorePropsFile.inputStream().use { keystoreProperties.load(it) }
}

android {
    compileSdk = 36
    namespace = "com.rp.chat"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.rp.chat"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    signingConfigs {
        getByName("debug") {
            // Keep the default debug signing for the debug buildType
        }
        if (keystoreProperties.containsKey("storeFile")) {
            create("release") {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            signingConfig = signingConfigs.findByName("release")
                ?: signingConfigs.getByName("debug")
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    // Kotlin/jni component of rustls-platform-verifier (provided by the local
    // maven repo above). Kept in sync with the crate via `latest.release`-style
    // resolution; the exposed version here must match the crate's.
    implementation("rustls:rustls-platform-verifier:0.1.1")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")