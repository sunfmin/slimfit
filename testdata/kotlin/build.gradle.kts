plugins {
    kotlin("jvm") version "2.4.21"
    id("org.jetbrains.kotlinx.kover") version "0.9.11"
    id("info.solidsoft.pitest") version "1.19.0"
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation(platform("org.junit:junit-bom:5.14.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

kotlin {
    jvmToolchain(21)
}

tasks.test {
    useJUnitPlatform()
}

pitest {
    pitestVersion.set("1.30.2")
    junit5PluginVersion.set("1.2.3")
    targetClasses.set(listOf("com.example.*"))
    outputFormats.set(listOf("XML"))
    timestampedReports.set(false)
    fullMutationMatrix.set(true)
}
