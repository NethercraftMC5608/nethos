/* What execve replaced the parent with. */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
	int i;

	printf("child: argc %d", argc);
	for (i = 0; i < argc; i++)
		printf(", argv[%d]=%s", i, argv[i]);
	printf("\n");
	printf("child: getenv(NK) is %s\n", getenv("NK"));
	return 9;
}
