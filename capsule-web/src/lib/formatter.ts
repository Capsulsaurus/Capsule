export const dateFormatter = new Intl.DateTimeFormat('en-US', {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    weekday: 'long',
    hour: 'numeric',
    minute: 'numeric',
    second: 'numeric',
}); // TODO: Load locale from settings

export const formatDate = (date: Date): string => {
    return dateFormatter.format(date);
};
