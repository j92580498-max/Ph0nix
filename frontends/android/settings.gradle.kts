pluginManagement {
    repositories {
        val googleMirror = providers.environmentVariable("FEATUREPHONE_GOOGLE_MAVEN").orNull
        if (googleMirror == null) google() else maven { url = uri(googleMirror) }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        val googleMirror = providers.environmentVariable("FEATUREPHONE_GOOGLE_MAVEN").orNull
        if (googleMirror == null) google() else maven { url = uri(googleMirror) }
        mavenCentral()
    }
}

rootProject.name = "FeaturePhoneEmulator"
include(":app")
