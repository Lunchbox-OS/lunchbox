# kotlinx.serialization keeps generated serializers via @Serializable;
# the plugin emits the required keep rules, but the reflective lookup of
# companion serializers needs these belt-and-suspenders rules when R8 is
# eventually enabled.
-keepclassmembers class **$$serializer { *; }
-keepclasseswithmembers class com.lunchbox_os.companion.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.lunchbox_os.companion.**$$serializer { *; }
