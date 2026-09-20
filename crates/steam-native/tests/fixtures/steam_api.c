// Independent C ABI fixture for the public SDK consumer. No Valve code or IPC.
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

static int user, apps, utils, stats, storage;
static bool initialized;

#ifndef MISSING_INIT
bool SteamAPI_Init(void) {
    initialized = true;
#ifdef LEGACY_INIT_FAIL
    return false;
#else
    return true;
#endif
}
#endif
void SteamAPI_Shutdown(void) { initialized = false; }
void SteamAPI_RunCallbacks(void) {}

#ifndef LEGACY
#ifndef MISSING_INIT
int SteamAPI_InitFlat(char *error) {
    (void)error;
#ifdef MODERN_INIT_FAIL
    return 2;
#else
    initialized = true;
    return 0;
#endif
}
#endif
#ifndef MISSING_USER
void *SteamAPI_SteamUser_v023(void) { return &user; }
#endif
void *SteamAPI_SteamApps_v008(void) { return &apps; }
void *SteamAPI_SteamUtils_v010(void) { return &utils; }
void *SteamAPI_SteamUserStats_v012(void) { return &stats; }
void *SteamAPI_SteamRemoteStorage_v016(void) { return &storage; }
#else
static int client;
void *SteamClient(void) { return initialized ? &client : 0; }
int SteamAPI_GetHSteamUser(void) { return initialized ? 17 : 0; }
int SteamAPI_GetHSteamPipe(void) { return initialized ? 29 : 0; }
static bool handles(void *object, int huser, int pipe) {
    return initialized && object == &client && huser == 17 && pipe == 29;
}
#ifndef MISSING_USER
void *SteamAPI_ISteamClient_GetISteamUser(void *object, int huser, int pipe, const char *version) {
    return handles(object, huser, pipe) && !strcmp(version, "SteamUser019") ? &user : 0;
}
#endif
void *SteamAPI_ISteamClient_GetISteamApps(void *object, int huser, int pipe, const char *version) {
    return handles(object, huser, pipe) && !strcmp(version, "STEAMAPPS_INTERFACE_VERSION008") ? &apps : 0;
}
void *SteamAPI_ISteamClient_GetISteamUtils(void *object, int pipe, const char *version) {
    return handles(object, 17, pipe) && !strcmp(version, "SteamUtils009") ? &utils : 0;
}
void *SteamAPI_ISteamClient_GetISteamUserStats(void *object, int huser, int pipe, const char *version) {
    return handles(object, huser, pipe) && !strcmp(version, "STEAMUSERSTATS_INTERFACE_VERSION011") ? &stats : 0;
}
void *SteamAPI_ISteamClient_GetISteamRemoteStorage(void *object, int huser, int pipe, const char *version) {
    return handles(object, huser, pipe) && !strcmp(version, "STEAMREMOTESTORAGE_INTERFACE_VERSION014") ? &storage : 0;
}
#endif

bool SteamAPI_ISteamUser_BLoggedOn(void *object) { return initialized && object == &user; }
uint64_t SteamAPI_ISteamUser_GetSteamID(void *object) { return object == &user ? 76561198000000001ULL : 0; }
bool SteamAPI_ISteamApps_BIsSubscribedApp(void *object, uint32_t app) { return object == &apps && app == 588650; }
uint32_t SteamAPI_ISteamUtils_GetAppID(void *object) { return object == &utils ? 588650 : 0; }
uint64_t SteamAPI_ISteamUserStats_GetNumberOfCurrentPlayers(void *object) { return object == &stats ? 123 : 0; }
bool SteamAPI_ISteamUtils_IsAPICallCompleted(void *object, uint64_t handle, bool *failed) {
    *failed = false;
    return object == &utils && handle == 123;
}
struct Players { uint8_t success; int32_t count; };
bool SteamAPI_ISteamUtils_GetAPICallResult(void *object, uint64_t handle, void *result, int size, int callback, bool *failed) {
    *failed = false;
    if (object != &utils || handle != 123 || size != 8 || callback != 1107) return false;
    *(struct Players *)result = (struct Players){1, 4};
    return true;
}
bool SteamAPI_ISteamRemoteStorage_GetQuota(void *object, uint64_t *total, uint64_t *available) {
    *total = 1000000;
    *available = 900000;
    return initialized && object == &storage;
}
int SteamAPI_ISteamRemoteStorage_GetFileCount(void *object) { return object == &storage ? 1 : 0; }
bool SteamAPI_ISteamRemoteStorage_FileExists(void *object, const char *name) {
    return object == &storage && !strcmp(name, "fixture.dat");
}
int SteamAPI_ISteamRemoteStorage_FileRead(void *object, const char *name, void *data, int size) {
    if (!SteamAPI_ISteamRemoteStorage_FileExists(object, name) || size != 4) return 0;
    memcpy(data, "save", 4);
    return 4;
}
