#include <stdio.h>
#include <string.h>

#ifndef ENTRY_NAME
#define ENTRY_NAME "entry-static.exe"
#endif

#ifdef HAVE_CRYPT
#include <crypt.h>
#endif

#ifdef HAVE_MUSL_PLEVAL
unsigned long __pleval(const char *, unsigned long);
#endif

static int t_status;

static void t_error(const char *msg)
{
    fputs(msg, stdout);
    t_status = 1;
}

#ifdef HAVE_CRYPT
static void check_crypt_value(const char *want, const char *salt, const char *key)
{
    char *got = crypt(key, salt);
    if (!got) {
        got = "*";
    }
    if (strcmp(got, want) != 0) {
        printf("crypt(\"%s\", \"%s\") failed: got \"%s\" want \"%s\"\n",
               key, salt, got, want);
        t_status = 1;
    }
}

#define T_CRYPT(h, s, k) check_crypt_value((h), (s), (k))

static int run_crypt_case(void)
{
    t_status = 0;

    T_CRYPT("$1$abcd0123$9Qcg8DyviekV3tDGMZynJ1", "$1$abcd0123$",
            "Xy01@#\x01\x02\x80\x7f\xff\r\n\x81\t !");
    T_CRYPT("$1$$qRPK7m23GJusamGpoGLby/", "$1$$", "");
    T_CRYPT("$1$salt$UsdFqFVB.FsuinRDK5eE..", "$1$salt$", "");
    T_CRYPT("$1$salt1234$.pylIeU8A8nhxsVrZNOP..", "$1$salt1234$",
            "Aa@\xaa 0123456789");
    T_CRYPT("$1$aaaaaaaa$zqksdEYCs/p2VrrMTPU0x0",
            "$1$aaaaaaaaaaaaaaaaaaaa$", "aaaaaaaaaaaaaaaaaaaa");

    T_CRYPT("*", "$2a$00$0123456789012345678901", "");
    T_CRYPT("*", "$2a$08$01234567890123456789", "");
    T_CRYPT("$2a$04$012345678901234567890u8auMTJmy9uQv1pCMPSGmRjXec5nzCf6",
            "$2a$04$0123456789012345678901", "");
    T_CRYPT("$2a$04$abcdefghijklmnopqrstuuEgxSMhZgdHqm5w1Iw6ZfXSn4If4J406",
            "$2a$04$abcdefghijklmnopqrstuv", "\xff\xff\xff\xa3\x33\x01\x40");
    T_CRYPT("$2a$04$abcdefghijklmnopqrstuu8J3SjO9LQpndv9O3HW/e0PB1xKk.PJu",
            "$2a$04$abcdefghijklmnopqrstuv", "Aa@\xaa 0123456789");
    T_CRYPT("$2x$04$abcdefghijklmnopqrstuubUAnPDiHn0JtKfNM4q6HN1ZsdaC1D8i",
            "$2x$04$abcdefghijklmnopqrstuv", "\xff\xff\xff\xa3\x33\x01\x40");
    T_CRYPT("$2x$04$abcdefghijklmnopqrstuuxYRr8W0rYwastFTc35iurVdXD9PtVhq",
            "$2x$04$abcdefghijklmnopqrstuv", "Aa@\xaa 0123456789");
    T_CRYPT("$2y$04$abcdefghijklmnopqrstuubUAnPDiHn0JtKfNM4q6HN1ZsdaC1D8i",
            "$2y$04$abcdefghijklmnopqrstuv", "\xff\xff\xff\xa3\x33\x01\x40");
    T_CRYPT("$2y$04$abcdefghijklmnopqrstuu8J3SjO9LQpndv9O3HW/e0PB1xKk.PJu",
            "$2y$04$abcdefghijklmnopqrstuv", "Aa@\xaa 0123456789");

    T_CRYPT("$5$$3c2QQ0KjIU1OLtB29cl8Fplc2WN7X89bnoEjaR7tWu.", "$5$$", "");
    T_CRYPT("$5$rounds=1234$abc0123456789$3VfDjPt05VHFn47C/ojFZ6KRPYrOjj1lLbH.dkF3bZ6",
            "$5$rounds=1234$abc0123456789$",
            "Xy01@#\x01\x02\x80\x7f\xff\r\n\x81\t !");
    T_CRYPT("$5$salt1234$1145V3OxW91Wl.LSS3pmBHvb2jV3ujiUhD7DgpoJtw9",
            "$5$salt1234$", "Aa@\xaa 0123456789");
    T_CRYPT("$5$rounds=1000$$ZIwsx59lFMWVo3Yt6IxpZVn0IhpY8Yg4gxC21zUDBI4",
            "$5$rounds=1$", "a");
    T_CRYPT("$5$rounds=1234$$i.IiuqtWmTzupHAZtfV/PB33Usz.MwGHq9BKFAEj.B3",
            "$5$rounds=00001234$", "a");
    T_CRYPT("$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5",
            "$5$saltstring", "Hello world!");
    T_CRYPT("$5$rounds=5000$toolongsaltstrin$Un/5jzAHMgOGZ5.mWJpuVolil07guHPvOW8mGRcvxa5",
            "$5$rounds=5000$toolongsaltstring", "This is just a test");
    T_CRYPT("$5$rounds=1400$anotherlongsalts$Rx.j8H.h8HjEDGomFU8bDkXm3XIUnzyxf12oP84Bnq1",
            "$5$rounds=1400$anotherlongsaltstring",
            "a very much longer text to encrypt.  This one even stretches over morethan one line.");
    T_CRYPT("$5$rounds=1000$roundstoolow$yfvwcWrQ8l/K0DAWyuPMDNHpIVlTQebY9l/gL972bIC",
            "$5$rounds=10$roundstoolow", "the minimum number is still observed");

    T_CRYPT("$6$$/chiBau24cE26QQVW3IfIe68Xu5.JQ4E8Ie7lcRLwqxO5cxGuBhqF2HmTL.zWJ9zjChg3yJYFXeGBQ2y3Ba1d1",
            "$6$$", "");
    T_CRYPT("$6$rounds=1234$abc0123456789$BCpt8zLrc/RcyuXmCDOE1ALqMXB2MH6n1g891HhFj8.w7LxGv.FTkqq6Vxc/km3Y0jE0j24jY5PIv/oOu6reg1",
            "$6$rounds=1234$abc0123456789$",
            "Xy01@#\x01\x02\x80\x7f\xff\r\n\x81\t !");
    T_CRYPT("$6$salt1234$44TYByJTJkEpcbmj8XzV6H7ltUN.7FUFFWKGeph85fMuAME8f1yQnXxqPbz6gfMq7tisOjTrxg3S2DDebWewt1",
            "$6$salt1234$", "Aa@\xaa 0123456789");
    T_CRYPT("$6$rounds=1000$$hETGMQQ5sXu1md3PrmRCM4AxTgbNpYQaIhk4xQzvCiNfeogfCR9PZGSRXghUOxMAPFU2wuz/ZLafIHrHopO.60",
            "$6$rounds=1$", "a");
    T_CRYPT("$6$rounds=1234$$.9spjeVb1fINMikxgAZEpur.ZQ/Gte./HuKWm2sAZ37eK3e1.ZdfRuatKdR/H..lKQfb2AB.RtHh7xKm.FE2J.",
            "$6$rounds=00001234$", "a");
    T_CRYPT("$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1",
            "$6$saltstring", "Hello world!");
    T_CRYPT("$6$rounds=5000$toolongsaltstrin$lQ8jolhgVRVhY4b5pZKaysCLi0QBxGoNeKQzQ3glMhwllF7oGDZxUhx1yxdYcz/e1JSbq3y6JMxxl8audkUEm0",
            "$6$rounds=5000$toolongsaltstring", "This is just a test");
    T_CRYPT("$6$rounds=1400$anotherlongsalts$POfYwTEok97VWcjxIiSOjiykti.o/pQs.wPvMxQ6Fm7I6IoYN3CmLs66x9t0oSwbtEW7o7UmJEiDwGqd8p4ur1",
            "$6$rounds=1400$anotherlongsaltstring",
            "a very much longer text to encrypt.  This one even stretches over morethan one line.");
    T_CRYPT("$6$rounds=1000$roundstoolow$kUMsbe306n21p9R.FRkW3IGn.S9NPN0x50YhH1xhLsPuWGsUSklZt58jaTfF4ZEQpyUNGc0dqbpBYYBaHHrsX.",
            "$6$rounds=10$roundstoolow", "the minimum number is still observed");

    return t_status;
}
#endif

#ifdef HAVE_MUSL_PLEVAL
static void check_pleval_value(const char *expr, unsigned long n, unsigned long want)
{
    unsigned long got = __pleval(expr, n);
    if (got != want) {
        printf("__pleval(\"%s\",%lu) failed: got %lu want %lu\n", expr, n, got, want);
        t_status = 1;
    }
}

#define T_PLEVAL(e) do { \
    unsigned long n; \
    for (n = 0; n < 200; n++) { \
        unsigned long want = (e); \
        check_pleval_value(#e ";", n, want); \
    } \
} while (0)

static int run_pleval_case(void)
{
    char buf[210];
    t_status = 0;

    memset(buf, '!', 200);
    memcpy(buf + 200, "n;", 3);
    check_pleval_value(buf, 7, (unsigned long)-1);

    memcpy(buf + 51, "n;", 3);
    check_pleval_value(buf, 3, 0);
    check_pleval_value(buf, 0, 1);
    memcpy(buf + 50, "n;", 3);
    check_pleval_value(buf, 3, 1);
    check_pleval_value(buf, 0, 0);

    check_pleval_value("!n n;", 1, (unsigned long)-1);
    check_pleval_value("32n;", 1, (unsigned long)-1);
    check_pleval_value("n/n;", 0, (unsigned long)-1);
    check_pleval_value("n*3-;", 1, (unsigned long)-1);
    check_pleval_value("4*;", 13, (unsigned long)-1);
    check_pleval_value("n?1:;", 13, (unsigned long)-1);

    T_PLEVAL(n % 4);
    T_PLEVAL(n == 1 || n == 2 || n % 9 == 7);
    T_PLEVAL((n == 1) + !n + (n == 3));
    T_PLEVAL(n - 13 - 5 + n * 3 / 7 - 8);
    T_PLEVAL(n + n > n == n - n < n ? n / (n || !!!n) : 0 - n);
    T_PLEVAL((n <= 3 >= 0) + n + n + n - n - n * 1 * 1 * 1 / 1 % 12345678);
    T_PLEVAL(5 < 6 - 4 * n && n % 3 == n - 1);
    T_PLEVAL(n % 7 && n || 0 && n - 1);

    T_PLEVAL(0);
    T_PLEVAL((n > 1));
    T_PLEVAL((n != 1));
    T_PLEVAL((n == 0 ? 0 : n == 1 ? 1 : n == 2 ? 2 : n % 100 >= 3 && n % 100 <= 10 ? 3 : n % 100 >= 11 ? 4 : 5));
    T_PLEVAL((n % 10 == 1 && n % 100 != 11 ? 0 : n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20) ? 1 : 2));
    T_PLEVAL((n == 1) ? 0 : (n >= 2 && n <= 4) ? 1 : 2);
    T_PLEVAL((n == 1) ? 0 : n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20) ? 1 : 2);
    T_PLEVAL((n == 1 ? 0 : n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20) ? 1 : 2));
    T_PLEVAL((n == 1) ? 0 : (n == 2) ? 1 : (n != 8 && n != 11) ? 2 : 3);
    T_PLEVAL((n == 1 ? 0 : (n == 0 || (n % 100 > 0 && n % 100 < 20)) ? 1 : 2));
    T_PLEVAL((n == 1) ? 0 : n == 2 ? 1 : n < 7 ? 2 : n < 11 ? 3 : 4);
    T_PLEVAL((n == 1 || n == 11) ? 0 : (n == 2 || n == 12) ? 1 : (n > 2 && n < 20) ? 2 : 3);
    T_PLEVAL((n % 10 != 1 || n % 100 == 11));
    T_PLEVAL((n != 0));
    T_PLEVAL((n == 1) ? 0 : (n == 2) ? 1 : (n == 3) ? 2 : 3);
    T_PLEVAL((n % 10 == 1 && n % 100 != 11 ? 0 : n != 0 ? 1 : 2));
    T_PLEVAL((n == 0 ? 0 : n == 1 ? 1 : 2));
    T_PLEVAL((n == 1 ? 0 : n == 0 || (n % 100 > 1 && n % 100 < 11) ? 1 : (n % 100 > 10 && n % 100 < 20) ? 2 : 3));
    T_PLEVAL((n == 1) ? 0 : (n >= 2 && n <= 4) ? 1 : 2);
    T_PLEVAL((n % 100 == 1 ? 1 : n % 100 == 2 ? 2 : n % 100 == 3 || n % 100 == 4 ? 3 : 0));

    return t_status;
}
#endif

static void print_start(const char *name)
{
    printf("========== START %s %s ==========\n", ENTRY_NAME, name);
}

static void print_end(const char *name)
{
    printf("========== END %s %s ==========\n", ENTRY_NAME, name);
}

static int run_named_case(const char *name)
{
    if (strcmp(name, "crypt") == 0) {
#ifdef HAVE_CRYPT
        return run_crypt_case();
#else
        t_error("crypt support was not linked into this extra runner\n");
        return 1;
#endif
    }
    if (strcmp(name, "pleval") == 0) {
#ifdef HAVE_MUSL_PLEVAL
        return run_pleval_case();
#else
        t_error("musl __pleval support is unavailable in this extra runner\n");
        return 1;
#endif
    }
    t_error("unknown libc-test extra case\n");
    return 1;
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        t_error("missing libc-test extra case name\n");
        return 1;
    }

    const char *name = argv[1];
    print_start(name);
    int status = run_named_case(name);
    if (status == 0) {
        puts("Pass!");
    } else {
        printf("FAIL %s [status %d]\n", name, status);
    }
    print_end(name);
    return status;
}
